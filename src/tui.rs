use std::{io::stdout, rc::Rc};

use crossterm::{
	event::{Event, KeyCode, MouseEventKind},
	execute,
	terminal::BeginSynchronizedUpdate
};
use ratatui::{
	layout::{Constraint, Flex, Layout, Rect},
	style::{Color, Style},
	text::Span,
	widgets::{Block, Borders, Padding},
	Frame
};
use ratatui_image::{protocol::Protocol, Image};

use crate::{renderer::RenderError, skip::Skip};

pub struct Tui {
	name: String,
	page: usize,
	last_render: LastRender,
	bottom_msg: BottomMessage,
	// we use `prev_msg` to, for example, restore the 'search results' message on the bottom after
	// jumping to a specific page
	prev_msg: Option<BottomMessage>,
	rendered: Vec<RenderedInfo>
}

#[derive(Default, Debug)]
struct LastRender {
	// Used as a way to track if we need to draw the images, to save ratatui from doing a lot of
	// diffing work
	rect: Rect,
	pages_shown: usize,
	unused_width: u16
}

#[derive(Default)]
enum BottomMessage {
	#[default]
	Help,
	SearchResults(String),
	Error(String),
	Input(InputCommand)
}

enum InputCommand {
	GoToPage(usize),
	Search(String)
}

// This seems like a kinda weird struct because it holds two optionals but any representation
// within it is valid; I think it's the best way to represent it
#[derive(Default)]
struct RenderedInfo {
	// The image, if it has been rendered by `Converter` to that struct
	img: Option<Box<dyn Protocol>>,
	// The number of results for the current search term that have been found on this page. None if
	// we haven't checked this page yet
	// Also this isn't the most efficient representation of this value, but it's accurate, so like
	// whatever I guess
	num_results: Option<usize>
}

impl Tui {
	pub fn new(name: String) -> Tui {
		Self {
			name,
			page: 0,
			prev_msg: None,
			bottom_msg: BottomMessage::Help,
			last_render: LastRender::default(),
			rendered: vec![]
		}
	}

	pub fn main_layout(frame: &Frame<'_>) -> Rc<[Rect]> {
		Layout::default()
			.constraints([
				Constraint::Length(3),
				Constraint::Fill(1),
				Constraint::Length(3)
			])
			.horizontal_margin(2)
			.vertical_margin(1)
			.split(frame.size())
	}

	// TODO: Make a way to fill the width of the screen with one page and scroll down to view it
	pub fn render(&mut self, frame: &mut Frame<'_>, main_area: &[Rect]) {
		let top_block = Block::new()
			.padding(Padding {
				right: 2,
				left: 2,
				..Padding::default()
			})
			.borders(Borders::BOTTOM);

		let top_area = top_block.inner(main_area[0]);

		let page_nums_text = format!("{} / {}", self.page + 1, self.rendered.len());

		let top_layout = Layout::horizontal([
			Constraint::Fill(1),
			Constraint::Length(page_nums_text.len() as u16)
		])
		.split(top_area);

		let title = Span::styled(&self.name, Style::new().fg(Color::Cyan));

		let page_nums = Span::styled(&page_nums_text, Style::new().fg(Color::Cyan));

		frame.render_widget(top_block, main_area[0]);
		frame.render_widget(title, top_layout[0]);
		frame.render_widget(page_nums, top_layout[1]);

		let bottom_block = Block::new()
			.padding(Padding {
				top: 1,
				right: 2,
				left: 2,
				bottom: 0
			})
			.borders(Borders::TOP);
		let bottom_area = bottom_block.inner(main_area[2]);

		frame.render_widget(bottom_block, main_area[2]);

		let rendered_str = if !self.rendered.is_empty() {
			format!(
				"Rendered: {}%",
				(self.rendered.iter().filter(|i| i.img.is_some()).count() * 100)
					/ self.rendered.len()
			)
		} else {
			String::new()
		};
		let bottom_layout = Layout::horizontal([
			Constraint::Fill(1),
			Constraint::Length(rendered_str.len() as u16)
		])
		.split(bottom_area);

		let rendered_span = Span::styled(&rendered_str, Style::new().fg(Color::Cyan));
		frame.render_widget(rendered_span, bottom_layout[1]);

		let (msg_str, color) = match self.bottom_msg {
			BottomMessage::Help => (
				"/: Search, g: Go To Page, n: Next Search Result, N: Previous Search Result"
					.to_string(),
				Color::Blue
			),
			BottomMessage::Error(ref e) => (format!("Couldn't render a page: {e}"), Color::Red),
			BottomMessage::Input(ref input_state) => (
				match input_state {
					InputCommand::GoToPage(page) => format!("Go to: {page}"),
					InputCommand::Search(s) => format!("Search: {s}")
				},
				Color::Blue
			),
			BottomMessage::SearchResults(ref term) => {
				let num_found = self
					.rendered
					.iter()
					.filter_map(|r| r.num_results)
					.sum::<usize>();
				let num_searched = self
					.rendered
					.iter()
					.filter(|r| r.num_results.is_some())
					.count() * 100;
				(
					format!(
						"Results for '{term}': {num_found} (searched: {}%)",
						num_searched / self.rendered.len()
					),
					Color::Blue
				)
			}
		};

		let span = Span::styled(msg_str, Style::new().fg(color));
		frame.render_widget(span, bottom_layout[0]);

		let mut img_area = main_area[1];

		let size = frame.size();
		if size == self.last_render.rect {
			// If we haven't resized (and haven't used the Rect as a way to mark that we need to
			// resize this time), then go through every element in the buffer where any Image would
			// be written and set to skip it so that ratatui doesn't spend a lot of time diffing it
			// each re-render
			frame.render_widget(Skip::new(true), img_area);
		} else {
			// here we calculate how many pages can fit in the available area.
			let mut test_area_w = img_area.width;
			// go through our pages, starting at the first one we want to view
			let page_widths = self.rendered[self.page..]
				.iter()
				// and get their indices (I know it's offset, we fix it down below when we actually
				// render each page)
				.enumerate()
				// and only take as many as are ready to be rendered
				.take_while(|(_, page)| page.img.is_some())
				// and map it to their width (in cells on the terminal, not pixels)
				.flat_map(|(idx, page)| page.img.as_ref().map(|img| (idx, img.rect().width)))
				// and then take them as long as they won't overflow the available area.
				.take_while(|(_, width)| match test_area_w.checked_sub(*width) {
					Some(new_val) => {
						test_area_w = new_val;
						true
					}
					None => false
				})
				.collect::<Vec<_>>();

			if page_widths.is_empty() {
				// If none are ready to render, just show the loading thing
				Self::render_loading_in(frame, img_area);
			} else {
				execute!(stdout(), BeginSynchronizedUpdate).unwrap();

				let total_width = page_widths.iter().map(|(_, w)| w).sum::<u16>();

				self.last_render.pages_shown = page_widths.len();

				let unused_width = img_area.width - total_width;
				self.last_render.unused_width = unused_width;
				img_area.x += unused_width / 2;

				for (page_idx, width) in page_widths {
					// now, theoretically, when we call this, this page should *not* be None, but we do
					// have to account for that possibility since we can't `borrow` the image from self
					// when passing it in to `render_single_page` since that would be a mutable
					// reference + an immutable reference (and also we need to potentially temporarily
					// remove it from the array of rendered pages to replace it with a text-rendered
					// image)
					self.render_single_page(frame, page_idx + self.page, Rect {
						width,
						..img_area
					});
					img_area.x += width;
				}

				// we want to set this at the very end so it doesn't get set somewhere halfway through and
				// then the whole diffing thing messes it up
				self.last_render.rect = size;
			}
		}
	}

	fn render_single_page(&mut self, frame: &mut Frame<'_>, page_idx: usize, img_area: Rect) {
		match self.rendered[page_idx].img {
			Some(ref page_img) => frame.render_widget(Image::new(&**page_img), img_area),
			None => Self::render_loading_in(frame, img_area)
		};
	}

	fn render_loading_in(frame: &mut Frame<'_>, area: Rect) {
		let loading_str = "Loading...";
		let inner_space = Layout::horizontal([Constraint::Length(loading_str.len() as u16)])
			.flex(Flex::Center)
			.split(area);

		let loading_span = Span::styled(loading_str, Style::new().fg(Color::Cyan));

		frame.render_widget(loading_span, inner_space[0]);
	}

	fn change_page(&mut self, change: PageChange, amt: ChangeAmount) -> Option<InputAction> {
		let diff = match amt {
			ChangeAmount::Single => 1,
			ChangeAmount::WholeScreen => self.last_render.pages_shown
		};

		let old = self.page;
		match change {
			PageChange::Next => self.set_page((self.page + diff).min(self.rendered.len() - 1)),
			PageChange::Prev => self.set_page(self.page.saturating_sub(diff))
		}

		match self.page as isize - old as isize {
			0 => None,
			_ => Some(InputAction::JumpingToPage(self.page))
		}
	}

	pub fn set_n_pages(&mut self, n_pages: usize) {
		self.rendered = Vec::with_capacity(n_pages);
		for _ in 0..n_pages {
			self.rendered.push(RenderedInfo::default());
		}
		self.page = self.page.min(n_pages - 1);
	}

	pub fn page_ready(&mut self, img: Box<dyn Protocol>, page_num: usize, num_results: usize) {
		// If this new image woulda fit within the available space on the last render AND it's
		// within the range where it might've been rendered with the last shown pages, then reset
		// the last rect marker so that all images are forced to redraw on next render and this one
		// is drawn with them
		if page_num >= self.page && page_num <= self.page + self.last_render.pages_shown {
			self.last_render.rect = Rect::default();
		} else {
			let img_w = img.rect().width;
			if img_w <= self.last_render.unused_width {
				let num_fit = self.last_render.unused_width / img_w;
				if page_num >= self.page && (self.page + num_fit as usize) >= page_num {
					self.last_render.rect = Rect::default();
				}
			}
		}

		// We always just set this here because we handle reloading in the `set_n_pages` function.
		// If the document was reloaded, then It'll have the `set_n_pages` called to set the new
		// number of pages, so the vec will already be cleared
		self.rendered[page_num] = RenderedInfo {
			img: Some(img),
			num_results: Some(num_results)
		};
	}

	pub fn got_num_results_on_page(&mut self, page_num: usize, num_results: usize) {
		self.rendered[page_num].num_results = Some(num_results);
	}

	pub fn handle_event(&mut self, ev: Event) -> Option<InputAction> {
		fn jump_to_page(
			page: &mut usize,
			rect: &mut Rect,
			new_page: Option<usize>
		) -> Option<InputAction> {
			new_page.map(|new_page| {
				*page = new_page;
				// Make sure we re-render
				*rect = Rect::default();
				InputAction::JumpingToPage(new_page)
			})
		}

		match ev {
			Event::Key(key) => {
				match key.code {
					KeyCode::Char(c)
						if let BottomMessage::Input(InputCommand::Search(ref mut term)) =
							self.bottom_msg =>
					{
						term.push(c);
						Some(InputAction::Redraw)
					}
					KeyCode::Backspace if let BottomMessage::Input(InputCommand::Search(ref mut term)) = self.bottom_msg => {
						term.pop();
						Some(InputAction::Redraw)
					},
					KeyCode::Char(c)
						if let BottomMessage::Input(InputCommand::GoToPage(ref mut page)) =
							self.bottom_msg =>
						c.to_digit(10).map(|input_num| {
							*page = (*page * 10) + input_num as usize;
							InputAction::Redraw
						}),
					KeyCode::Right | KeyCode::Char('l') =>
						self.change_page(PageChange::Next, ChangeAmount::Single),
					KeyCode::Down | KeyCode::Char('j') =>
						self.change_page(PageChange::Next, ChangeAmount::WholeScreen),
					KeyCode::Left | KeyCode::Char('h') =>
						self.change_page(PageChange::Prev, ChangeAmount::Single),
					KeyCode::Up | KeyCode::Char('k') =>
						self.change_page(PageChange::Prev, ChangeAmount::WholeScreen),
					KeyCode::Esc => match self.bottom_msg {
						BottomMessage::Input(_) => {
							self.set_bottom_msg(None);
							Some(InputAction::Redraw)
						}
						_ => Some(InputAction::QuitApp)
					},
					KeyCode::Char('q') => Some(InputAction::QuitApp),
					KeyCode::Char('g') => {
						self.set_bottom_msg(Some(BottomMessage::Input(InputCommand::GoToPage(0))));
						Some(InputAction::Redraw)
					}
					KeyCode::Char('/') => {
						self.set_bottom_msg(Some(BottomMessage::Input(InputCommand::Search(
							String::new()
						))));
						Some(InputAction::Redraw)
					}
					KeyCode::Char('n') if self.page < self.rendered.len() - 1 => {
						// TODO: If we can't find one, then maybe like block until we've verified
						// all the pages have been checked?
						let next_page = self.rendered[(self.page + 1)..]
							.iter()
							.enumerate()
							.find_map(|(idx, p)| {
								p.num_results
									.is_some_and(|num| num > 0)
									.then_some(self.page + 1 + idx)
							});

						jump_to_page(&mut self.page, &mut self.last_render.rect, next_page)
					}
					KeyCode::Char('N') if self.page > 0 => {
						let prev_page = self.rendered[..(self.page)]
							.iter()
							.rev()
							.enumerate()
							.find_map(|(idx, p)| {
								p.num_results
									.is_some_and(|num| num > 0)
									.then_some(self.page - (idx + 1))
							});

						jump_to_page(&mut self.page, &mut self.last_render.rect, prev_page)
					}
					KeyCode::Enter => {
						let BottomMessage::Input(_) = self.bottom_msg else {
							return None;
						};

						self.set_bottom_msg(None);
						let Some(BottomMessage::Input(ref cmd)) = self.prev_msg else {
							// We need to verify it's an input msg currently, and only then take it
							// and replace it by a default Help message. Don't exactly know how to
							// do this otherwise.
							unreachable!();
						};

						match cmd {
							// Only forward the command if it's within range
							InputCommand::GoToPage(page) => {
								let page = *page;
								(page < self.rendered.len()).then(|| {
									self.set_page(page);
									InputAction::JumpingToPage(page)
								})
							}
							InputCommand::Search(term) => {
								let term = term.clone();

								// We only want to show search results if there would actually be
								// data to show
								if !term.is_empty() {
									self.set_bottom_msg(Some(BottomMessage::SearchResults(
										term.clone()
									)));
								} else {
									// else, if it's not empty, we just want to reset the bottom
									// area to show the default data; we don't want it to like show
									// the data from a previous search
									self.set_bottom_msg(Some(BottomMessage::Help));
								}

								// Reset all the search results
								for img in &mut self.rendered {
									img.num_results = None;
								}
								// but we still want to tell the rest of the system that we set the
								// search term to '' so that they can re-render the pages wthout
								// the highlighting
								Some(InputAction::Search(term))
							}
						}
					}
					_ => None
				}
			}
			Event::Mouse(mouse) => match mouse.kind {
				MouseEventKind::ScrollRight =>
					self.change_page(PageChange::Next, ChangeAmount::Single),
				MouseEventKind::ScrollDown =>
					self.change_page(PageChange::Next, ChangeAmount::WholeScreen),
				MouseEventKind::ScrollLeft =>
					self.change_page(PageChange::Prev, ChangeAmount::Single),
				MouseEventKind::ScrollUp =>
					self.change_page(PageChange::Prev, ChangeAmount::WholeScreen),
				_ => None
			},
			// One of these options is Event::Resize, and we don't care about that because
			// we always check, regardless, if the available area for the images has
			// changed.
			_ => None
		}
	}

	pub fn show_error(&mut self, err: RenderError) {
		self.set_bottom_msg(Some(BottomMessage::Error(match err {
			RenderError::Doc(e) => format!("Couldn't open document: {e}"),
			RenderError::Render(e) => format!("Couldn't render page: {e}")
		})));
	}

	fn set_page(&mut self, page: usize) {
		if page != self.page {
			// mark that we need to re-render the images
			self.last_render.rect = Rect::default();
			self.page = page;
		}
	}

	// We have `msg` as optional so that if they reset it to none, it'll replace it with
	// `prev_msg`, but if they reset it to something else, it'll put the current thing in prev_msg
	fn set_bottom_msg(&mut self, msg: Option<BottomMessage>) {
		match msg {
			Some(mut msg) => {
				std::mem::swap(&mut self.bottom_msg, &mut msg);
				self.prev_msg = Some(msg);
			}
			None => {
				let mut new_bottom = self.prev_msg.take().unwrap_or_default();
				std::mem::swap(&mut self.bottom_msg, &mut new_bottom);
				self.prev_msg = Some(new_bottom);
			}
		}
	}
}

pub enum InputAction {
	Redraw,
	JumpingToPage(usize),
	Search(String),
	QuitApp
}

enum PageChange {
	Prev,
	Next
}

enum ChangeAmount {
	WholeScreen,
	Single
}
