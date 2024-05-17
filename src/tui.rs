use std::{io::stdout, rc::Rc};

use crossterm::{event::{Event, KeyCode, MouseEventKind}, execute, terminal::BeginSynchronizedUpdate};
use ratatui::{layout::{Constraint, Flex, Layout, Rect}, style::{Color, Style}, text::Span, widgets::{Block, Borders, Padding}, Frame};
use ratatui_image::{protocol::Protocol, Image};

use crate::{renderer::RenderError, skip::Skip};

pub struct Tui {
	name: String,
	page: usize,
	error: Option<String>,
	input_state: Option<InputCommand>,
	last_render: LastRender,
	rendered: Vec<Option<Box<dyn Protocol>>>,
}

#[derive(Default, Debug)]
struct LastRender {
	// Used as a way to track if we need to draw the images, to save ratatui from doing a lot of
	// diffing work
	rect: Rect,
	pages_shown: usize,
	unused_width: u16
}

enum InputCommand {
	GoToPage(usize)
}

impl Tui {
	pub fn new(name: String) -> Tui {
		Self {
			name,
			page: 0,
			error: None,
			input_state: None,
			last_render: LastRender::default(),
			rendered: vec![],
		}
	}

	pub fn main_layout(frame: &Frame<'_>) -> Rc<[Rect]> {
		Layout::default()
			.constraints([
				Constraint::Length(3),
				Constraint::Fill(1),
				Constraint::Length(3)
			])
			.horizontal_margin(4)
			.vertical_margin(2)
			.split(frame.size())
	}

	pub fn render(&mut self, frame: &mut Frame<'_>, main_area: &[Rect], end_update: &mut bool) {
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
		]).split(top_area);

		let title = Span::styled(
			&self.name,
			Style::new()
				.fg(Color::Cyan)
		);

		let page_nums = Span::styled(
			&page_nums_text,
			Style::new()
				.fg(Color::Cyan)
		);

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

		let rendered_str = format!(
			"Rendered: {}%",
			(self.rendered.iter().filter(|i| i.is_some()).count() * 100) / self.rendered.len()
		);

		let bottom_layout = Layout::horizontal([
			Constraint::Fill(1),
			Constraint::Length(rendered_str.len() as u16)
		]).split(bottom_area);

		let rendered_span = Span::styled(
			&rendered_str,
			Style::new()
				.fg(Color::Cyan)
		);
		frame.render_widget(rendered_span, bottom_layout[1]);

		if let Some(ref error_str) = self.error {
			let span = Span::styled(
				format!("Couldn't render a page: {error_str}"),
				Style::new()
					.fg(Color::Red)
			);
			frame.render_widget(span, bottom_layout[0]);
		} else if let Some(ref cmd) = self.input_state {
			match cmd {
				InputCommand::GoToPage(page) => {
					let span = Span::styled(
						format!("Go to: {page}"),
						Style::new()
							.fg(Color::Blue)
					);
					frame.render_widget(span, bottom_layout[0]);
				}
			}
		}

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
			let page_widths = self.rendered[self.page..].iter()
				// and get their indices (I know it's offset, we fix it down below when we actually
				// render each page)
				.enumerate()
				// and only take as many as are ready to be rendered
				.take_while(|(_, page)| page.is_some())
				// and map it to their width (in cells on the terminal, not pixels)
				.flat_map(|(idx, page)|
					page.as_ref().map(|img| (
						idx,
						img.rect().width,
					))
				)
				// and then take them as long as they won't overflow the available area.
				.take_while(|(_, width)| {
					match test_area_w.checked_sub(*width) {
						Some(new_val) => {
							test_area_w = new_val;
							true
						},
						None => false
					}
				})
				.collect::<Vec<_>>();

			if page_widths.is_empty() {
				// If none are ready to render, just show the loading thing
				Self::render_loading_in(frame, img_area);
			} else {
				execute!(stdout(), BeginSynchronizedUpdate).unwrap();
				*end_update = true;

				let total_width = page_widths
					.iter()
					.map(|(_, w)| w)
					.sum::<u16>();

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
					self.render_single_page(frame, page_idx + self.page, Rect { width, ..img_area });
					img_area.x += width;
				}

				// we want to set this at the very end so it doesn't get set somewhere halfway through and
				// then the whole diffing thing messes it up
				self.last_render.rect = size;
			}
		}
	}

	fn render_single_page(&mut self, frame: &mut Frame<'_>, page_idx: usize, img_area: Rect) {
		match self.rendered[page_idx] {
			Some(ref page_img) => frame.render_widget(Image::new(&**page_img), img_area),
			None => Self::render_loading_in(frame, img_area)
		};
	}

	fn render_loading_in(frame: &mut Frame<'_>, area: Rect) {
		let loading_str = "Loading...";
		let inner_space = Layout::horizontal([
			Constraint::Length(loading_str.len() as u16),
		]).flex(Flex::Center)
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
			PageChange::Prev => self.set_page(self.page.saturating_sub(diff)),
		}

		match self.page as isize - old as isize {
			0 => None,
			change => Some(InputAction::ChangePageBy(change))
		}
	}

	pub fn set_n_pages(&mut self, n_pages: usize) {
		self.rendered = Vec::with_capacity(n_pages);
		for _ in 0..n_pages {
			self.rendered.push(None);
		}
		self.page = self.page.min(n_pages - 1);
	}

	pub fn page_ready(&mut self, img: Box<dyn Protocol>, page_num: usize) {
		// If this new image woulda fit within the available space on the last render AND it's
		// within the range where it might've been rendered with the last shown pages, then reset
		// the last rect marker so that all images are forced to redraw on next render and this one
		// is drawn with them
		if page_num == self.page {
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
		self.rendered[page_num] = Some(img);
	}

	pub fn handle_event(&mut self, ev: Event) -> Option<InputAction> {
		match ev {
			Event::Key(key) => {
				match key.code {
					KeyCode::Right | KeyCode::Char('l') => self.change_page(PageChange::Next, ChangeAmount::Single),
					KeyCode::Down | KeyCode::Char('j') => self.change_page(PageChange::Next, ChangeAmount::WholeScreen),
					KeyCode::Left | KeyCode::Char('h') => self.change_page(PageChange::Prev, ChangeAmount::Single),
					KeyCode::Up | KeyCode::Char('k') => self.change_page(PageChange::Prev, ChangeAmount::WholeScreen),
					KeyCode::Esc | KeyCode::Char('q') => Some(InputAction::QuitApp),
					KeyCode::Char('g') => {
						self.input_state = Some(InputCommand::GoToPage(0));
						Some(InputAction::Redraw)
					},
					KeyCode::Char(c) => {
						let Some(InputCommand::GoToPage(ref mut page)) = self.input_state else {
							return None;
						};

						c.to_digit(10)
							.map(|input_num| {
								*page = (*page * 10) + input_num as usize;
								InputAction::Redraw
							})
					},
					KeyCode::Enter => self.input_state.take()
						.and_then(|cmd| match cmd {
							// Only forward the command if it's within range
							InputCommand::GoToPage(page) => (page < self.rendered.len()).then(|| {
								self.set_page(page);
								InputAction::JumpingToPage(page)
							})
						}),
					_ => None,
				}
			},
			Event::Mouse(mouse) => match mouse.kind {
				MouseEventKind::ScrollRight => self.change_page(PageChange::Next, ChangeAmount::Single),
				MouseEventKind::ScrollDown => self.change_page(PageChange::Next, ChangeAmount::WholeScreen),
				MouseEventKind::ScrollLeft => self.change_page(PageChange::Prev, ChangeAmount::Single),
				MouseEventKind::ScrollUp => self.change_page(PageChange::Prev, ChangeAmount::WholeScreen),
				_ => None,
			}
			// One of these options is Event::Resize, and we don't care about that because
			// we always check, regardless, if the available area for the images has
			// changed.
			_ => None,
		}
	}

	pub fn show_error(&mut self, err: RenderError) {
		self.error = Some(match err {
			RenderError::Doc(e) => format!("Couldn't open document: {e}"),
			RenderError::Render(e) => format!("Couldn't render page: {e}")
		});
	}

	fn set_page(&mut self, page: usize) {
		if page != self.page {
			// mark that we need to re-render the images
			self.last_render.rect = Rect::default();
			self.page = page;
		}
	}
}

pub enum InputAction {
	Redraw,
	ChangePageBy(isize),
	JumpingToPage(usize),
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
