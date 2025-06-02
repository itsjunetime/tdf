use std::{borrow::Cow, io::stdout, num::NonZeroUsize};

use crossterm::{
	event::{Event, KeyCode, KeyModifiers, MouseEventKind},
	execute,
	terminal::{
		BeginSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
		enable_raw_mode
	}
};
use nix::{
	sys::signal::{Signal::SIGSTOP, kill},
	unistd::Pid
};
use ratatui::{
	Frame,
	layout::{Constraint, Flex, Layout, Rect},
	style::{Color, Style},
	symbols::border,
	text::{Span, Text},
	widgets::{Block, Borders, Clear, Padding}
};
use ratatui_image::Image;

use crate::{
	converter::{ConvertedImage, MaybeTransferred},
	renderer::{RenderError, fill_default},
	skip::Skip
};

pub struct Tui {
	name: String,
	page: usize,
	last_render: LastRender,
	bottom_msg: BottomMessage,
	// we use `prev_msg` to, for example, restore the 'search results' message on the bottom after
	// jumping to a specific page
	prev_msg: Option<BottomMessage>,
	rendered: Vec<RenderedInfo>,
	page_constraints: PageConstraints,
	showing_help_msg: bool
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
pub enum BottomMessage {
	#[default]
	Help,
	SearchResults(String),
	Error(String),
	Input(InputCommand),
	Reloaded
}

pub enum InputCommand {
	GoToPage(usize),
	Search(String)
}

struct PageConstraints {
	max_wide: Option<NonZeroUsize>,
	r_to_l: bool
}

// This seems like a kinda weird struct because it holds two optionals but any representation
// within it is valid; I think it's the best way to represent it
#[derive(Default)]
struct RenderedInfo {
	// The image, if it has been rendered by `Converter` to that struct
	img: Option<ConvertedImage>,
	// The number of results for the current search term that have been found on this page. None if
	// we haven't checked this page yet
	// Also this isn't the most efficient representation of this value, but it's accurate, so like
	// whatever I guess
	num_results: Option<usize>
}

#[derive(PartialEq)]
pub struct RenderLayout {
	pub page_area: Rect,
	pub top_and_bottom: Option<(Rect, Rect)>
}

impl Tui {
	pub fn new(name: String, max_wide: Option<NonZeroUsize>, r_to_l: bool) -> Tui {
		Self {
			name,
			page: 0,
			prev_msg: None,
			bottom_msg: BottomMessage::Help,
			last_render: LastRender::default(),
			rendered: vec![],
			page_constraints: PageConstraints { max_wide, r_to_l },
			showing_help_msg: false
		}
	}

	pub fn main_layout(frame: &Frame<'_>, fullscreened: bool) -> RenderLayout {
		if fullscreened {
			RenderLayout {
				page_area: frame.area(),
				top_and_bottom: None
			}
		} else {
			let layout = Layout::default()
				.constraints([
					Constraint::Length(3),
					Constraint::Fill(1),
					Constraint::Length(3)
				])
				.horizontal_margin(2)
				.vertical_margin(1)
				.split(frame.area());

			RenderLayout {
				page_area: layout[1],
				top_and_bottom: Some((layout[0], layout[2]))
			}
		}
	}

	// TODO: Make a way to fill the width of the screen with one page and scroll down to view it
	#[must_use]
	pub fn render<'s>(
		&'s mut self,
		frame: &mut Frame<'_>,
		full_layout: &RenderLayout
	) -> Vec<(usize, &'s mut MaybeTransferred, Rect)> {
		if self.showing_help_msg {
			self.render_help_msg(frame);
			return vec![];
		}

		if let Some((top_area, bottom_area)) = full_layout.top_and_bottom {
			let top_block = Block::new()
				.padding(Padding {
					right: 2,
					left: 2,
					..Padding::default()
				})
				.borders(Borders::BOTTOM);

			let top_area = top_block.inner(top_area);

			let page_nums_text = format!("{} / {}", self.page + 1, self.rendered.len());

			let top_layout = Layout::horizontal([
				Constraint::Fill(1),
				Constraint::Length(page_nums_text.len() as u16)
			])
			.split(top_area);

			let title = Span::styled(&self.name, Style::new().fg(Color::Cyan));

			let page_nums = Span::styled(&page_nums_text, Style::new().fg(Color::Cyan));

			frame.render_widget(top_block, top_area);
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
			let bottom_inside_block = bottom_block.inner(bottom_area);

			frame.render_widget(bottom_block, bottom_area);

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
			.split(bottom_inside_block);

			let rendered_span = Span::styled(&rendered_str, Style::new().fg(Color::Cyan));
			frame.render_widget(rendered_span, bottom_layout[1]);

			let (msg_str, color): (Cow<'_, str>, _) = match self.bottom_msg {
				BottomMessage::Help => ("?: Show help page".into(), Color::Blue),
				BottomMessage::Error(ref e) => (e.as_str().into(), Color::Red),
				BottomMessage::Input(ref input_state) => (
					match input_state {
						InputCommand::GoToPage(page) => format!("Go to: {page}"),
						InputCommand::Search(s) => format!("Search: {s}")
					}
					.into(),
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
						)
						.into(),
						Color::Blue
					)
				}
				BottomMessage::Reloaded => ("Document was reloaded!".into(), Color::Blue)
			};

			let span = Span::styled(msg_str, Style::new().fg(color));
			frame.render_widget(span, bottom_layout[0]);
		}

		let mut img_area = full_layout.page_area;

		let size = frame.area();
		if size == self.last_render.rect {
			// If we haven't resized (and haven't used the Rect as a way to mark that we need to
			// resize this time), then go through every element in the buffer where any Image would
			// be written and set to skip it so that ratatui doesn't spend a lot of time diffing it
			// each re-render
			frame.render_widget(Skip::new(true), img_area);
			vec![]
		} else {
			// here we calculate how many pages can fit in the available area.
			let mut test_area_w = img_area.width;
			// go through our pages, starting at the first one we want to view
			let mut page_widths = self.rendered[self.page..]
				.iter_mut()
				// and get this to represent a count of how many we're looking at so far to render
				.enumerate()
				// and only take as many as are ready to be rendered
				.take_while(|(idx, page)| {
					let mut take = page.img.is_some();
					if let Some(max) = self.page_constraints.max_wide {
						take &= *idx < max.get();
					}
					take
				})
				// and map it to their width (in cells on the terminal, not pixels)
				.filter_map(|(_, page)| page.img.as_mut().map(|img| (img.area().width, img)))
				// and then take them as long as they won't overflow the available area.
				.take_while(|(width, _)| match test_area_w.checked_sub(*width) {
					Some(new_val) => {
						test_area_w = new_val;
						true
					}
					None => false
				})
				.collect::<Vec<_>>();

			if self.page_constraints.r_to_l {
				page_widths.reverse();
			}

			if page_widths.is_empty() {
				// If none are ready to render, just show the loading thing
				Self::render_loading_in(frame, img_area);
				vec![]
			} else {
				execute!(stdout(), BeginSynchronizedUpdate).unwrap();

				let total_width = page_widths.iter().map(|(w, _)| w).sum::<u16>();

				self.last_render.pages_shown = page_widths.len();

				let unused_width = img_area.width - total_width;
				self.last_render.unused_width = unused_width;
				img_area.x += unused_width / 2;

				let to_display = page_widths
					.into_iter()
					.enumerate()
					.filter_map(|(idx, (width, img))| {
						let maybe_img =
							Self::render_single_page(frame, img, Rect { width, ..img_area });
						img_area.x += width;
						maybe_img.map(|(img, r)| (idx + self.page, img, r))
					})
					.collect::<Vec<_>>();

				// we want to set this at the very end so it doesn't get set somewhere halfway through and
				// then the whole diffing thing messes it up
				self.last_render.rect = size;

				to_display
			}
		}
	}

	fn render_single_page<'img>(
		frame: &mut Frame<'_>,
		page_img: &'img mut ConvertedImage,
		img_area: Rect
	) -> Option<(&'img mut MaybeTransferred, Rect)> {
		match page_img {
			ConvertedImage::Generic(page_img) => {
				frame.render_widget(Image::new(page_img), img_area);
				None
			}
			ConvertedImage::Kitty { img, area } => Some((img, Rect {
				x: img_area.x,
				y: img_area.y,
				width: area.width,
				height: area.height
			}))
		}
	}

	fn render_loading_in(frame: &mut Frame<'_>, area: Rect) {
		let loading_str = "Loading...";
		let inner_space = Layout::horizontal([Constraint::Length(loading_str.len() as u16)])
			.flex(Flex::Center)
			.split(area);

		let loading_span = Span::styled(loading_str, Style::new().fg(Color::Cyan));

		frame.render_widget(loading_span, inner_space[0]);
	}

	fn change_page(&mut self, mut change: PageChange, amt: ChangeAmount) -> Option<InputAction> {
		let diff = match amt {
			ChangeAmount::Single => 1,
			ChangeAmount::WholeScreen => self.last_render.pages_shown
		};

		// This is a kinda weird way to switch around the controls for this sort of thing but it
		// allows it to be pretty centralized and avoids annoyingly duplicated match arms (since
		// we'd have to do `match key { 'h' if r_to_l | 'l' => {}}` and that doesn't play well with
		// `if` guards on match arms)
		if self.page_constraints.r_to_l {
			change = match change {
				PageChange::Next => PageChange::Prev,
				PageChange::Prev => PageChange::Next
			};
		}

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
		fill_default(&mut self.rendered, n_pages);
		self.page = self.page.min(n_pages - 1);
	}

	pub fn page_ready(&mut self, img: ConvertedImage, page_num: usize, num_results: usize) {
		// If this new image woulda fit within the available space on the last render AND it's
		// within the range where it might've been rendered with the last shown pages, then reset
		// the last rect marker so that all images are forced to redraw on next render and this one
		// is drawn with them
		if page_num >= self.page && page_num <= self.page + self.last_render.pages_shown {
			self.last_render.rect = Rect::default();
		} else {
			let img_w = img.area().width;
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

	pub fn page_failed_display(&mut self, page_num: usize) {
		self.rendered[page_num].img = None;
	}

	pub fn got_num_results_on_page(&mut self, page_num: usize, num_results: usize) {
		self.rendered[page_num].num_results = Some(num_results);
	}

	pub fn handle_event(&mut self, ev: &Event) -> Option<InputAction> {
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
					KeyCode::Char(c) => {
						// TODO: refactor back to `if let` arm guards when those are stabilized
						if let BottomMessage::Input(InputCommand::Search(ref mut term)) =
							self.bottom_msg
						{
							term.push(c);
							return Some(InputAction::Redraw);
						}

						if let BottomMessage::Input(InputCommand::GoToPage(ref mut page)) =
							self.bottom_msg
						{
							return c.to_digit(10).map(|input_num| {
								*page = (*page * 10) + input_num as usize;
								InputAction::Redraw
							});
						}

						match c {
							'l' => self.change_page(PageChange::Next, ChangeAmount::Single),
							'j' => self.change_page(PageChange::Next, ChangeAmount::WholeScreen),
							'h' => self.change_page(PageChange::Prev, ChangeAmount::Single),
							'k' => self.change_page(PageChange::Prev, ChangeAmount::WholeScreen),
							'q' => Some(InputAction::QuitApp),
							'g' => {
								self.set_msg(MessageSetting::Some(BottomMessage::Input(
									InputCommand::GoToPage(0)
								)));
								Some(InputAction::Redraw)
							}
							'/' => {
								self.set_msg(MessageSetting::Some(BottomMessage::Input(
									InputCommand::Search(String::new())
								)));
								Some(InputAction::Redraw)
							}
							'i' => Some(InputAction::Invert),
							'?' => {
								self.showing_help_msg = true;
								Some(InputAction::Redraw)
							}
							'f' => Some(InputAction::Fullscreen),
							'n' if self.page < self.rendered.len() - 1 => {
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
							'N' if self.page > 0 => {
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
							'z' if key.modifiers.contains(KeyModifiers::CONTROL) => {
								// [todo] better error handling here?

								let mut backend = stdout();
								execute!(
									&mut backend,
									LeaveAlternateScreen,
									crossterm::cursor::Show
								)
								.unwrap();
								disable_raw_mode().unwrap();

								// This process will hang after the SIGSTOP call until we get
								// foregrounded again by something else, at which point we need to
								// re-setup everything so that it all gets drawn again.
								kill(Pid::this(), SIGSTOP).unwrap();

								enable_raw_mode().unwrap();
								execute!(
									&mut backend,
									EnterAlternateScreen,
									crossterm::cursor::Hide
								)
								.unwrap();

								self.last_render.rect = Rect::default();
								Some(InputAction::Redraw)
							}
							_ => None
						}
					}
					KeyCode::Backspace => {
						if let BottomMessage::Input(InputCommand::Search(ref mut term)) =
							self.bottom_msg
						{
							term.pop();
							return Some(InputAction::Redraw);
						}
						None
					}
					KeyCode::Right => self.change_page(PageChange::Next, ChangeAmount::Single),
					KeyCode::Down => self.change_page(PageChange::Next, ChangeAmount::WholeScreen),
					KeyCode::Left => self.change_page(PageChange::Prev, ChangeAmount::Single),
					KeyCode::Up => self.change_page(PageChange::Prev, ChangeAmount::WholeScreen),
					KeyCode::Esc => match (self.showing_help_msg, &self.bottom_msg) {
						(false, BottomMessage::Help) => Some(InputAction::QuitApp),
						_ => {
							// When we hit escape, we just want to pop off the current message and
							// show the underlying one.
							self.set_msg(MessageSetting::Pop);
							Some(InputAction::Redraw)
						}
					},
					KeyCode::Enter => {
						let mut default = BottomMessage::default();
						std::mem::swap(&mut self.bottom_msg, &mut default);
						let BottomMessage::Input(ref cmd) = default else {
							std::mem::swap(&mut self.bottom_msg, &mut default);
							return None;
						};

						match cmd {
							// Only forward the command if it's within range
							InputCommand::GoToPage(page) => {
								// We need to subtract 1 b/c they're tracked internally as
								// 0-indexed but input and displayed as 1-indexed
								let zero_page = page.saturating_sub(1);
								let rendered_len = self.rendered.len();

								if zero_page < rendered_len {
									self.set_page(zero_page);
									Some(InputAction::JumpingToPage(zero_page))
								} else {
									self.set_msg(MessageSetting::Some(BottomMessage::Error(
										format!(
											"Cannot jump to page {page}; there are only {rendered_len} pages in the document"
										)
									)));
									Some(InputAction::Redraw)
								}
							}
							InputCommand::Search(term) => {
								let term = term.clone();

								// We only want to show search results if there would actually be
								// data to show
								if !term.is_empty() {
									self.set_msg(MessageSetting::Some(
										BottomMessage::SearchResults(term.clone())
									));
								} else {
									// else, if it's not empty, we just want to reset the bottom
									// area to show the default data; we don't want it to like show
									// the data from a previous search
									self.set_msg(MessageSetting::Reset);
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
			Event::Resize(_, _) => Some(InputAction::Redraw),
			_ => None
		}
	}

	pub fn show_error(&mut self, err: RenderError) {
		self.set_msg(MessageSetting::Some(BottomMessage::Error(match err {
			RenderError::Notify(e) => format!("Auto-reload failed: {e}"),
			RenderError::Doc(e) => format!("Couldn't process document: {e}"),
			RenderError::Converting(e) => format!("Couldn't convert page after rendering: {e}")
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
	pub fn set_msg(&mut self, msg: MessageSetting) {
		match msg {
			MessageSetting::Some(mut msg) => {
				std::mem::swap(&mut self.bottom_msg, &mut msg);
				self.prev_msg = Some(msg);
			}
			MessageSetting::Default => self.set_msg(MessageSetting::Some(BottomMessage::default())),
			MessageSetting::Reset => {
				self.prev_msg = None;
				self.bottom_msg = BottomMessage::default();
			}
			MessageSetting::Pop =>
				if self.showing_help_msg {
					self.last_render.rect = Rect::default();
					self.showing_help_msg = false;
				} else {
					self.bottom_msg = self.prev_msg.take().unwrap_or_default();
				},
		}
	}

	pub fn render_help_msg(&self, frame: &mut Frame<'_>) {
		let frame_area = frame.area();
		frame.render_widget(Clear, frame_area);

		let block = Block::new()
			.title("Help")
			.padding(Padding::proportional(1))
			.borders(Borders::ALL)
			.border_set(border::ROUNDED)
			.border_style(Color::Blue);

		let help_span = Text::raw(HELP_PAGE);

		let max_w: u16 = HELP_PAGE
			.lines()
			.map(str::len)
			.max()
			.unwrap_or_default()
			.try_into()
			.expect("Every help text line must be shorter than u16::MAX");

		let layout = Layout::horizontal([
			Constraint::Fill(1),
			Constraint::Length(max_w + 6),
			Constraint::Fill(1)
		])
		.split(frame_area);

		let block_area = Layout::vertical([
			Constraint::Fill(1),
			Constraint::Length(u16::try_from(HELP_PAGE.lines().count()).unwrap() + 4),
			Constraint::Fill(1)
		])
		.split(layout[1]);

		let block_inner = block.inner(block_area[1]);

		frame.render_widget(block, block_area[1]);
		frame.render_widget(help_span, block_inner);
	}
}

static HELP_PAGE: &str = "\
l, h, left, right:
	Go forward/backwards a single page
j, k, down, up:
	Go forwards/backwards a screen's worth of pages
q, esc:
	Quit
g:
	Go to specific page (type numbers after 'g')
/:
	Search
n, N:
	Next/Previous search result
i:
	Invert colors
f:
	Remove borders/fullscreen
?:
	Show this page
ctrl+z:
	Suspend & background tdf \
";

pub enum InputAction {
	Redraw,
	JumpingToPage(usize),
	Search(String),
	QuitApp,
	Invert,
	Fullscreen
}

#[derive(Copy, Clone)]
enum PageChange {
	Prev,
	Next
}

#[derive(Copy, Clone)]
enum ChangeAmount {
	WholeScreen,
	Single
}

pub enum MessageSetting {
	Some(BottomMessage),
	Default,
	Reset,
	Pop
}
