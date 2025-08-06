use std::{borrow::Cow, io::stdout, num::NonZeroUsize};

use crossterm::{
	event::{Event, KeyCode, KeyModifiers, MouseEventKind},
	execute,
	terminal::{
		BeginSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
		enable_raw_mode
	}
};
use kittage::display::DisplayLocation;
use nix::{
	sys::signal::{Signal::SIGSTOP, kill},
	unistd::Pid
};
use ratatui::{
	Frame,
	layout::{Constraint, Flex, Layout, Position, Rect},
	style::{Color, Style},
	symbols::border,
	text::{Span, Text},
	widgets::{Block, Borders, Clear, Padding}
};
use ratatui_image::{FontSize, Image};

use crate::{
	FitOrFill,
	converter::{ConvertedImage, MaybeTransferred},
	kitty::{KittyDisplay, KittyReadyToDisplay},
	renderer::{RenderError, fill_default},
	skip::Skip
};

pub struct Tui {
	name: String,
	pub page: usize,
	last_render: LastRender,
	bottom_msg: BottomMessage,
	// we use `prev_msg` to, for example, restore the 'search results' message on the bottom after
	// jumping to a specific page
	prev_msg: Option<BottomMessage>,
	rendered: Vec<RenderedInfo>,
	page_constraints: PageConstraints,
	showing_help_msg: bool,
	is_kitty: bool,
	zoom: Option<Zoom>
}

#[derive(Default)]
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

#[derive(Default, Debug)]
struct Zoom {
	// just how much 'zoom' you have. Doesn't relate to anything specific yet, except that 0 means
	// it fills the screen (instead of fits)
	level: i16,
	// how many terminal-cells worth of content overflow the left side of the screen (and are thus
	// not displayed)
	cell_pan_from_left: u16,
	// how many terminal-cells worth of content overflow the top side of the screen (and are thus
	// not displayed)
	cell_pan_from_top: u16
}

// This seems like a kinda weird struct because it holds two optionals but any representation
// within it is valid; I think it's the best way to represent it
#[derive(Default)]
pub struct RenderedInfo {
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
	pub fn new(name: String, max_wide: Option<NonZeroUsize>, r_to_l: bool, is_kitty: bool) -> Tui {
		Self {
			name,
			page: 0,
			prev_msg: None,
			bottom_msg: BottomMessage::Help,
			last_render: LastRender::default(),
			rendered: vec![],
			page_constraints: PageConstraints { max_wide, r_to_l },
			showing_help_msg: false,
			is_kitty,
			zoom: None
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
		full_layout: &RenderLayout,
		font_size: FontSize
	) -> KittyDisplay<'s> {
		if self.showing_help_msg {
			self.render_help_msg(frame);
			return KittyDisplay::ClearImages;
		}

		if let Some(t_and_b) = full_layout.top_and_bottom {
			Self::render_top_and_bottom(
				t_and_b,
				self.page,
				&self.rendered,
				&self.name,
				frame,
				&self.bottom_msg
			);
		}

		let mut img_area = full_layout.page_area;

		let size = frame.area();
		if size == self.last_render.rect {
			// If we haven't resized (and haven't used the Rect as a way to mark that we need to
			// resize this time), then go through every element in the buffer where any Image would
			// be written and set to skip it so that ratatui doesn't spend a lot of time diffing it
			// each re-render
			frame.render_widget(Skip::new(true), img_area);
			KittyDisplay::NoChange
		} else {
			if let Some(ref mut zoom) = self.zoom {
				// yes this is ugly and I hate it. it's due to the limitations that currently exist
				// in the borrow checker. Once `-Zpolonius=next` is stabilized, we can rework this
				// to look like what we expect.
				// See https://github.com/rust-lang/rfcs/blob/master/text/2094-nll.md#problem-case-3-conditional-control-flow-across-functions
				// You can also rewrite this to just if an `if let` and run it under
				// `RUSTFLAGS="-Zpolonius=next"` and see that it works
				if self.rendered[self.page]
					.img
					.as_ref()
					.is_some_and(|c| matches!(c, ConvertedImage::Kitty { .. }))
				{
					let Some(ConvertedImage::Kitty {
						ref mut img,
						cell_w,
						cell_h
					}) = self.rendered[self.page].img
					else {
						unreachable!()
					};

					log::debug!("zoom is now {zoom:#?}");
					log::debug!("img_area is {img_area:#?}");

					if zoom.level < 0 {
						img_area = Rect {
							width: img_area
								.width
								.saturating_sub((zoom.level * 2).unsigned_abs())
								.max(1),
							x: img_area.x + (zoom.level.unsigned_abs().min(img_area.width / 2)),
							..img_area
						}
					}

					log::debug!("after adjustment, img_area is {img_area:#?}");

					// Ugh I don't like this logic. I wish we could simplify it.
					let img_width = f32::from(cell_w);
					let img_height = f32::from(cell_h);
					let img_area_width = f32::from(img_area.width);
					let img_area_height = f32::from(img_area.height);
					let available_to_real_width_ratio = img_area_width / img_width;
					let available_to_real_height_ratio = img_area_height / img_height;

					let (new_cell_width, new_cell_height) =
						if available_to_real_width_ratio > available_to_real_height_ratio {
							(img_width, img_area_height / available_to_real_width_ratio)
						} else {
							(img_area_width / available_to_real_height_ratio, img_height)
						};

					log::debug!("new_cell stuff is {new_cell_width}x{new_cell_height}");

					let width = (new_cell_width * f32::from(font_size.0)) as u32;
					let height = (new_cell_height * f32::from(font_size.1)) as u32;

					self.last_render = LastRender {
						rect: size,
						pages_shown: 1,
						unused_width: 0
					};

					zoom.cell_pan_from_left = zoom
						.cell_pan_from_left
						.min(cell_w.saturating_sub(new_cell_width as u16));
					zoom.cell_pan_from_top = zoom
						.cell_pan_from_top
						.min(cell_h.saturating_sub(new_cell_height as u16));

					return KittyDisplay::DisplayImages(vec![KittyReadyToDisplay {
						img,
						page_num: self.page,
						pos: Position {
							x: img_area.x,
							y: img_area.y
						},
						display_loc: DisplayLocation {
							x: u32::from(zoom.cell_pan_from_left) * u32::from(font_size.0),
							y: u32::from(zoom.cell_pan_from_top) * u32::from(font_size.1),
							width,
							height,
							columns: img_area.width,
							rows: img_area.height,
							..DisplayLocation::default()
						}
					}]);
				}
			};

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
				.filter_map(|(_, page)| page.img.as_mut().map(|img| (img.w_h().0, img)))
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
				KittyDisplay::ClearImages
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
						maybe_img.map(|(img, pos)| KittyReadyToDisplay {
							img,
							page_num: idx + self.page,
							pos,
							display_loc: DisplayLocation::default()
						})
					})
					.collect::<Vec<_>>();

				// we want to set this at the very end so it doesn't get set somewhere halfway through and
				// then the whole diffing thing messes it up
				self.last_render.rect = size;

				KittyDisplay::DisplayImages(to_display)
			}
		}
	}

	fn render_single_page<'img>(
		frame: &mut Frame<'_>,
		page_img: &'img mut ConvertedImage,
		img_area: Rect
	) -> Option<(&'img mut MaybeTransferred, Position)> {
		match page_img {
			ConvertedImage::Generic(page_img) => {
				frame.render_widget(Image::new(page_img), img_area);
				None
			}
			ConvertedImage::Kitty {
				img,
				cell_h: _,
				cell_w: _
			} => Some((img, Position {
				x: img_area.x,
				y: img_area.y
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
			PageChange::Next =>
				self.set_page((self.page + diff).min(self.rendered.len().saturating_sub(1))),
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
			let img_w = img.w_h().0;
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

	pub fn render_top_and_bottom(
		(top_area, bottom_area): (Rect, Rect),
		page_num: usize,
		rendered: &[RenderedInfo],
		doc_name: &str,
		frame: &mut Frame<'_>,
		bottom_msg: &BottomMessage
	) {
		// use the extra space here to add some padding to the right side
		let page_nums_text = format!("{} / {} ", page_num + 1, rendered.len());

		let top_block = Block::new()
			// use this first title to add a bit of padding to the left side
			.title_top(" ")
			.title_top(Span::styled(doc_name, Style::new().fg(Color::Cyan)))
			.title_top(
				Span::styled(&page_nums_text, Style::new().fg(Color::Cyan))
					.into_right_aligned_line()
			)
			.padding(Padding {
				bottom: 1,
				..Padding::default()
			})
			.borders(Borders::BOTTOM);

		frame.render_widget(top_block, top_area);

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

		let rendered_str = if !rendered.is_empty() {
			format!(
				"Rendered: {}%",
				(rendered.iter().filter(|i| i.img.is_some()).count() * 100) / rendered.len()
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

		let (msg_str, color): (Cow<'_, str>, _) = match bottom_msg {
			BottomMessage::Help => ("?: Show help page".into(), Color::Blue),
			BottomMessage::Error(e) => (e.as_str().into(), Color::Red),
			BottomMessage::Input(input_state) => (
				match input_state {
					InputCommand::GoToPage(page) => format!("Go to: {page}"),
					InputCommand::Search(s) => format!("Search: {s}")
				}
				.into(),
				Color::Blue
			),
			BottomMessage::SearchResults(term) => {
				let num_found = rendered.iter().filter_map(|r| r.num_results).sum::<usize>();
				let num_searched =
					rendered.iter().filter(|r| r.num_results.is_some()).count() * 100;
				(
					format!(
						"Results for '{term}': {num_found} (searched: {}%)",
						num_searched / rendered.len()
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
							'z' if self.is_kitty => {
								let (zoom, f_or_f) = match self.zoom {
									None => (Some(Zoom::default()), FitOrFill::Fill),
									Some(_) => (None, FitOrFill::Fit)
								};
								self.zoom = zoom;
								self.last_render.rect = Rect::default();
								Some(InputAction::SwitchRenderZoom(f_or_f))
							}
							'o' if self.is_kitty => self.update_zoom(|z|
								// TODO: for now, we don't let people zoom in past fill-screen
								z.level = z.level.saturating_add(1).min(0)),
							'O' if self.is_kitty =>
								self.update_zoom(|z| z.level = z.level.saturating_sub(1)),
							'L' if self.is_kitty => self.update_zoom(|z| {
								z.cell_pan_from_left = z.cell_pan_from_left.saturating_add(1)
							}),
							'H' if self.is_kitty => self.update_zoom(|z| {
								z.cell_pan_from_left = z.cell_pan_from_left.saturating_sub(1)
							}),
							'J' if self.is_kitty => self.update_zoom(|z| {
								z.cell_pan_from_top = z.cell_pan_from_top.saturating_add(1)
							}),
							'K' if self.is_kitty => self.update_zoom(|z| {
								z.cell_pan_from_top = z.cell_pan_from_top.saturating_sub(1)
							}),
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

	// I want this to always return 0 'cause I just use it to return from `Self::handle_event`]
	#[expect(clippy::unnecessary_wraps)]
	fn update_zoom(&mut self, f: impl FnOnce(&mut Zoom)) -> Option<InputAction> {
		if let Some(z) = &mut self.zoom {
			f(z)
		}
		self.last_render.rect = Rect::default();
		Some(InputAction::Redraw)
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
	Fullscreen,
	SwitchRenderZoom(crate::FitOrFill)
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
