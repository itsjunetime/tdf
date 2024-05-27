use cairo::{Antialias, Format};
use crossterm::terminal::WindowSize;
use itertools::Itertools;
use poppler::{Color, Document, FindFlags, Page, Rectangle, SelectionStyle};
use ratatui::layout::Rect;
use tokio::sync::mpsc::{error::TryRecvError, UnboundedReceiver, UnboundedSender};

pub enum RenderNotif {
	Area(Rect),
	JumpToPage(usize),
	Search(String),
	Reload
}

#[derive(Debug)]
pub enum RenderError {
	Doc(glib::Error),
	// Don't like storing an error as a string but it needs to be Send to send to the main thread,
	// and it's just going to be shown to the user, so whatever
	Render(String)
}

pub enum RenderInfo {
	NumPages(usize),
	Page(PageInfo)
}

pub struct PageInfo {
	pub img_data: ImageData,
	pub page: usize,
	pub search_results: usize
}

pub struct ImageData {
	pub data: Vec<u8>,
	pub area: Rect
}

#[derive(Default)]
struct PrevRender {
	successful: bool,
	contained_term: Option<bool>
}

pub fn fill_default<T: Default>(vec: &mut Vec<T>, size: usize) {
	vec.clear();
	vec.reserve(size.saturating_sub(vec.len()));
	for _ in 0..size {
		vec.push(T::default());
	}
}

// this function has to be sync (non-async) because the poppler::Document needs to be held during
// most of it, but that's basically just a wrapper around `*c_void` cause it's just a binding to C
// code, so it's !Send and thus can't be held across await points. So we can't call any of the
// async `send` or `recv` methods in this function body, since those create await points. Which
// means we need to call blocking_(send|recv). Those functions panic if called in an async context.
// So here we are.
// Also we just kinda 'unwrap' all of the send/recv calls here 'cause if they return an error, that
// means the other side's disconnected, which means that the main thread has panicked, which means
// we're done.
pub fn start_rendering(
	path: String,
	sender: UnboundedSender<Result<RenderInfo, RenderError>>,
	mut receiver: UnboundedReceiver<RenderNotif>,
	size: WindowSize
) {
	// first, wait 'til we get told what the current starting area is so that we can set it to
	// know what to render to
	let mut area;
	loop {
		if let RenderNotif::Area(r) = receiver.blocking_recv().unwrap() {
			area = r;
			break;
		}
	}

	// We want this outside of 'reload so that if the doc reloads, the search term that somebody
	// set will still get highlighted in the reloaded doc
	let mut search_term = None;

	'reload: loop {
		let doc = match Document::from_file(&path, None) {
			Err(e) => {
				sender.send(Err(RenderError::Doc(e))).unwrap();
				return;
			}
			Ok(d) => d
		};

		let n_pages = doc.n_pages() as usize;
		sender.send(Ok(RenderInfo::NumPages(n_pages))).unwrap();

		// We're using this vec of bools to indicate which page numbers have already been rendered,
		// to support people jumping to specific pages and having quick rendering results. We
		// `split_at_mut` at 0 initially (which bascially makes `right == rendered && left == []`),
		// doing basically nothing, but if we get a notification that something has been jumped to,
		// then we can split at that page and render at both sides of it
		let mut rendered = vec![];
		fill_default::<PrevRender>(&mut rendered, n_pages);
		let mut start_point = 0;

		// This is kinda a weird way of doing this, but if we get a notification that the area
		// changed, we want to start re-rending all of the pages, but we don't want to reload the
		// document. If there was a mechanism to say 'start this for-loop over' then I would do
		// that, but I don't think such a thing exists, so this is our attempt
		'render_pages: loop {
			// what we do with a notif is the same regardless of if we're in the middle of
			// rendering the list of pages or we're all done
			macro_rules! handle_notif {
				($notif:ident) => {
					match $notif {
						RenderNotif::Reload => continue 'reload,
						RenderNotif::Area(new_area) => {
							let bigger =
								new_area.width > area.width || new_area.height > area.height;
							area = new_area;
							// we only want to re-render pages if the new area is greater than the old
							// one, 'cause then we might need sharper images to make it all look good.
							// If the new area is smaller, then the same high-quality-rendered images
							// will still look fine, so it's ok to leave it.
							if bigger {
								fill_default(&mut rendered, n_pages);
								continue 'render_pages;
							}
						}
						RenderNotif::JumpToPage(page) => {
							start_point = page;
							continue 'render_pages;
						}
						RenderNotif::Search(term) => {
							if term.is_empty() {
								// If the term is set to nothing, then we don't need to re-render
								// the pages wherein there were already no search results. So this
								// is a little optimization to allow that.
								for page in &mut rendered {
									if !page.successful || page.contained_term != Some(true) {
										page.successful = false;
									}
								}
								search_term = None;
							} else {
								// But if the term is set to something new, we need to reset all of
								// the 'contained_term' fields so that if they now contain the
								// term, we can render them with the term, but if they don't, we
								// don't need to re-render and send it over again.
								for page in &mut rendered {
									page.contained_term = None;
								}
								search_term = Some(term);
							}
							continue 'render_pages;
						}
					}
				};
			}

			let (left, right) = rendered.split_at_mut(start_point);

			let page_iter = right
				.iter_mut()
				.enumerate()
				.map(|(idx, p)| (idx + start_point, p))
				.interleave(
					left.iter_mut()
						.rev()
						.enumerate()
						.map(|(idx, p)| (start_point - (idx + 1), p))
				);

			// we go through each page
			for (num, rendered) in page_iter {
				// we only want to continue if one of the following is met:
				// 1. It failed to render last time (we want to retry)
				// 2. The `contained_term` is set to None (representing 'Unknown'), meaning that we
				//    need to at least check if it contains the current term to see if it needs a
				//    re-render
				if rendered.successful && rendered.contained_term.is_some() {
					continue;
				}

				// check if we've been told to change the area that we're rendering to,
				// or if we're told to rerender
				match receiver.try_recv() {
					Err(TryRecvError::Disconnected) => panic!("disconnected :("),
					Ok(notif) => handle_notif!(notif),
					Err(TryRecvError::Empty) => ()
				};

				// We know this is in range 'cause we're iterating over it
				let Some(page) = doc.page(num as i32) else {
					sender
						.send(Err(RenderError::Render(format!(
							"Couldn't get page {num} ({}) of doc?",
							num as i32
						))))
						.unwrap();
					continue;
				};

				let rendered_with_no_results =
					rendered.successful && rendered.contained_term == Some(false);

				// render the page
				match render_single_page(
					page,
					area,
					num,
					&search_term,
					rendered_with_no_results,
					&size
				) {
					// If we've already rendered it just fine and we don't need to render it again,
					// just continue. We're all good
					Ok(None) => (),
					// If that fn returned Some, that means it needed to be re-rendered for some
					// reason or another, so we're sending it here
					Ok(Some(img)) => {
						// But we first need to store if we already rendered it correctly so that
						// the next time we iterate through, it might see that we're already good
						rendered.contained_term = Some(img.search_results > 0);
						rendered.successful = true;
						sender.send(Ok(RenderInfo::Page(img))).unwrap()
					}
					// And if we got an error, then obviously we need to propagate that
					Err(e) => sender.send(Err(RenderError::Render(e))).unwrap()
				}
			}
			// Then once we've rendered all these pages, wait until we get another notification
			// that this doc needs to be reloaded
			loop {
				// This once returned None despite the main thing being still connected (I think, at
				// last), so I'm just being safe here
				let Some(msg) = receiver.blocking_recv() else {
					return;
				};
				handle_notif!(msg);
			}
		}
	}
}

fn render_single_page(
	page: Page,
	area: Rect,
	page_num: usize,
	search_term: &Option<String>,
	already_rendered_no_results: bool,
	size: &WindowSize
) -> Result<Option<PageInfo>, String> {
	let mut result_rects = search_term
		.as_ref()
		.map(|term| page.find_text_with_options(term, FindFlags::DEFAULT | FindFlags::MULTILINE))
		.unwrap_or_default();

	// If there are no search terms on this page, and we've already rendered it with no search
	// terms, then just return none to avoid this computation
	if result_rects.is_empty() && already_rendered_no_results {
		return Ok(None);
	}

	// First, get the font size; the number of pixels (width x height) per font character (I
	// think; it's at least something like that) on this terminal screen.
	let col_h = size.height / size.rows;
	let col_w = size.width / size.columns;

	// then, get the size of the page
	let (p_width, p_height) = page.size();

	// and get its aspect ratio
	let p_aspect_ratio = p_width / p_height;

	// Then we get the full pixel dimensions of the area provided to us, and the aspect ratio
	// of that area
	let area_full_h = (area.height * col_h) as f64;
	let area_full_w = (area.width * col_w) as f64;
	let area_aspect_ratio = area_full_w / area_full_h;

	// and get the ratio that this page would have to be scaled by to fit perfectly within the
	// area provided to us.
	// we do this first by comparing the aspec ratio of the page with the aspect ratio of the
	// area to fit it within. If the aspect ratio of the page is larger, then we need to scale
	// the width of the page to fill perfectly within the height of the area. Otherwise, we
	// scale the height to fit perfectly. The dimension that _is not_ scaled to fit perfectly
	// is scaled by the same factor as the dimension that _is_ scaled perfectly.
	let scale_factor = if p_aspect_ratio > area_aspect_ratio {
		area_full_w / p_width
	} else {
		area_full_h / p_height
	};

	let surface_width = p_width * scale_factor;
	let surface_height = p_height * scale_factor;

	let surface = cairo::ImageSurface::create(
		Format::ARgb32,
		// No matter how big you make these arguments, the image will be drawn at the same
		// size. So if you make them really big, the image will be drawn on a quarter of it. If
		// you make them really small, the image will cover more than all of the surface.
		//
		// However, that only stands as long as you don't scale the context that you place this
		// surface into. If you scale the dimensions of this image by n, then scale the context
		// by that same amount, then it'll still fit perfectly into the context, but be
		// rendered at higher quality.
		surface_width as i32,
		surface_height as i32
	)
	.map_err(|e| format!("Couldn't create ImageSurface: {e}"))?;
	let ctx = cairo::Context::new(surface).map_err(|e| format!("Couldn't create Context: {e}"))?;

	ctx.scale(scale_factor, scale_factor);

	// The default background color of PDFs (at least, I think) is white, so we need to set
	// that as the background color, then paint, then render.
	ctx.set_source_rgba(1.0, 1.0, 1.0, 1.0);

	ctx.set_antialias(Antialias::Best);
	ctx.paint()
		.map_err(|e| format!("Couldn't paint Context: {e}"))?;
	page.render(&ctx);

	let num_results = result_rects.len();

	let mut highlight_color = Color::new();
	highlight_color.set_red((u16::MAX / 5) * 4);
	highlight_color.set_green((u16::MAX / 5) * 4);

	let mut old_rect = Rectangle::new();
	for rect in result_rects.iter_mut() {
		// According to https://gitlab.freedesktop.org/poppler/poppler/-/issues/763, these rects
		// need to be corrected since they use different references as the y-coordinate base
		rect.set_y1(p_height - rect.y1());
		rect.set_y2(p_height - rect.y2());

		page.render_selection(
			&ctx,
			rect,
			&mut old_rect,
			SelectionStyle::Glyph,
			&mut Color::new(),
			&mut highlight_color
		);
	}

	ctx.scale(1. / scale_factor, 1. / scale_factor);

	let mut img_data = Vec::new();
	ctx.target()
		.write_to_png(&mut img_data)
		.map_err(|e| format!("Couldn't write surface to png: {e}"))?;

	Ok(Some(PageInfo {
		img_data: ImageData {
			data: img_data,
			area: Rect {
				width: surface_width as u16 / col_w,
				height: surface_height as u16 / col_h,
				..Rect::default()
			}
		},
		page: page_num,
		search_results: num_results
	}))
}
