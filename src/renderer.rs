use std::thread;

use cairo::{Antialias, Context, Format, Surface};
use crossterm::terminal::WindowSize;
use flume::{Receiver, SendError, Sender, TryRecvError};
use itertools::Itertools;
use poppler::{Color, Document, FindFlags, Page, Rectangle, SelectionStyle};
use ratatui::layout::Rect;

pub enum RenderNotif {
	Area(Rect),
	JumpToPage(usize),
	Search(String),
	Reload
}

#[derive(Debug)]
pub enum RenderError {
	Notify(notify::Error),
	Doc(glib::Error),
	// Don't like storing an error as a string but it needs to be Send to send to the main thread,
	// and it's just going to be shown to the user, so whatever
	Render(String)
}

pub enum RenderInfo {
	NumPages(usize),
	Page(PageInfo)
}

#[derive(Clone)]
pub struct PageInfo {
	pub img_data: ImageData,
	pub page: usize,
	pub search_results: usize
}

#[derive(Clone)]
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
// We're allowing passing by value here because this is only called once, at the beginning of the
// program, and the arguments that 'should' be passed by value (`receiver` and `size`) would
// probably be more performant if accessed by-value instead of through a reference. Probably.
#[allow(clippy::needless_pass_by_value)]
pub fn start_rendering(
	path: &str,
	mut sender: Sender<Result<RenderInfo, RenderError>>,
	receiver: Receiver<RenderNotif>,
	size: WindowSize
) -> Result<(), SendError<Result<RenderInfo, RenderError>>> {
	// first, wait 'til we get told what the current starting area is so that we can set it to
	// know what to render to
	let mut area = loop {
		if let RenderNotif::Area(r) = receiver.recv().unwrap() {
			break r;
		}
	};

	// We want this outside of 'reload so that if the doc reloads, the search term that somebody
	// set will still get highlighted in the reloaded doc
	let mut search_term = None;

	// And although the font size could theoretically change, we aren't accounting for that right
	// now, so we just keep this out of the loop.
	let col_w = size.width / size.columns;
	let col_h = size.height / size.rows;

	'reload: loop {
		let doc = match Document::from_file(path, None) {
			Err(e) => {
				// if there's an error, tell the main loop
				sender.send(Err(RenderError::Doc(e)))?;
				// then wait for a reload notif (since what probably happened is that the file was
				// temporarily removed to facilitate a save or something like that)
				while let Ok(msg) = receiver.recv() {
					// and once that comes, just try to reload again
					if let RenderNotif::Reload = msg {
						continue 'reload;
					}
				}
				// if that while let Ok ever fails and we exit out of that loop, the main thread is
				// done, so we're fine to just return
				return Ok(());
			}
			Ok(d) => d
		};

		let n_pages = doc.n_pages() as usize;
		sender.send(Ok(RenderInfo::NumPages(n_pages)))?;

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

			let area_w = f64::from(area.width) * f64::from(col_w);
			let area_h = f64::from(area.height) * f64::from(col_h);

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
					// If it's disconnected, then the main loop is done, so we should just give up
					Err(TryRecvError::Disconnected) => return Ok(()),
					Ok(notif) => handle_notif!(notif),
					Err(TryRecvError::Empty) => ()
				};

				// We know this is in range 'cause we're iterating over it but we still just want
				// to be safe
				let Some(page) = doc.page(num as i32) else {
					sender.send(Err(RenderError::Render(format!(
						"Couldn't get page {num} ({}) of doc?",
						num as i32
					))))?;
					continue;
				};

				let rendered_with_no_results =
					rendered.successful && rendered.contained_term == Some(false);

				// render the page
				match render_single_page_to_ctx(
					&page,
					search_term.as_deref(),
					rendered_with_no_results,
					(area_w, area_h)
				) {
					// If we've already rendered it just fine and we don't need to render it again,
					// just continue. We're all good
					Ok(None) => (),
					// If that fn returned Some, that means it needed to be re-rendered for some
					// reason or another, so we're sending it here
					Ok(Some(ctx)) => {
						// we make a potentially incorrect assumption here that writing the context
						// to a png won't fail, and mark that it all rendered correctly here before
						// spawning off the thread to do so and send it.
						rendered.contained_term = Some(ctx.num_results > 0);
						rendered.successful = true;

						// if this is the page that the user is currently trying to look at, don't
						// bother spawning off a thread to render it to a png - it'll only slow
						// down the time til the user can see it (due to the overhead of creating a
						// thread), but we still want to spawn threads to render the other pages
						// since the effects of parallelizing that will be noticeable if the user
						// tries to move through pages more quickly
						if num == start_point {
							render_ctx_to_png(&ctx, &mut sender, (col_w, col_h), num)?;
						} else {
							let mut sender = sender.clone();
							thread::spawn(move || {
								render_ctx_to_png(&ctx, &mut sender, (col_w, col_h), num)
							});
						}
					}
					// And if we got an error, then obviously we need to propagate that
					Err(e) => sender.send(Err(RenderError::Render(e)))?
				}
			}

			// Then once we've rendered all these pages, wait until we get another notification
			// that this doc needs to be reloaded
			loop {
				// This once returned None despite the main thing being still connected (I think, at
				// least), so I'm just being safe here
				let Ok(msg) = receiver.recv() else {
					return Ok(());
				};
				handle_notif!(msg);
			}
		}
	}
}

struct RenderedContext {
	surface: Surface,
	num_results: usize,
	surface_width: f64,
	surface_height: f64
}

/// SAFETY: I think this is safe because, although the backing struct for `Surface` does contain
/// pointers to like the cairo_backend_t struct that all the cairo stuff is using, that struct is
/// basically just a vtable, so accessing it from multiple threads *should* be safe since we're
/// just calling the same functions with different data. The only other thing it holds reference to
/// is a `cairo_device_t`, but that seems to be thread-safe because it's managed through ref counts
/// and a mutex. Also, as far as I can tell from reading the source code, write_to_png_stream (the
/// only function we call on this struct) doesn't access the device at all, so we should be fine
/// there.
/// We want this to be Send so that we can delegate the png writing to a separate thread (since
/// that's the thing that takes the most time, by far, in this app).
unsafe impl Send for RenderedContext {}

fn render_single_page_to_ctx(
	page: &Page,
	search_term: Option<&str>,
	already_rendered_no_results: bool,
	(area_w, area_h): (f64, f64)
) -> Result<Option<RenderedContext>, String> {
	let mut result_rects = search_term
		.as_ref()
		.map(|term| page.find_text_with_options(term, FindFlags::DEFAULT | FindFlags::MULTILINE))
		.unwrap_or_default();

	// If there are no search terms on this page, and we've already rendered it with no search
	// terms, then just return none to avoid this computation
	if result_rects.is_empty() && already_rendered_no_results {
		return Ok(None);
	}

	// then, get the size of the page
	let (p_width, p_height) = page.size();

	// and get its aspect ratio
	let p_aspect_ratio = p_width / p_height;

	// Then we get the full pixel dimensions of the area provided to us, and the aspect ratio
	// of that area
	let area_aspect_ratio = area_w / area_h;

	// and get the ratio that this page would have to be scaled by to fit perfectly within the
	// area provided to us.
	// we do this first by comparing the aspec ratio of the page with the aspect ratio of the
	// area to fit it within. If the aspect ratio of the page is larger, then we need to scale
	// the width of the page to fill perfectly within the height of the area. Otherwise, we
	// scale the height to fit perfectly. The dimension that _is not_ scaled to fit perfectly
	// is scaled by the same factor as the dimension that _is_ scaled perfectly.
	let scale_factor = if p_aspect_ratio > area_aspect_ratio {
		area_w / p_width
	} else {
		area_h / p_height
	};

	let surface_width = p_width * scale_factor;
	let surface_height = p_height * scale_factor;

	let surface = cairo::ImageSurface::create(
		Format::Rgb16_565,
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
	surface.set_device_scale(scale_factor, scale_factor);

	let ctx = Context::new(surface).map_err(|e| format!("Couldn't create Context: {e}"))?;

	// The default background color of PDFs (at least, I think) is white, so we need to set
	// that as the background color, then paint, then render.
	ctx.set_source_rgba(1.0, 1.0, 1.0, 1.0);

	ctx.set_antialias(Antialias::None);
	ctx.paint()
		.map_err(|e| format!("Couldn't paint Context: {e}"))?;
	page.render(&ctx);

	let num_results = result_rects.len();

	if !result_rects.is_empty() {
		let mut highlight_color = Color::new();
		highlight_color.set_red((u16::MAX / 5) * 4);
		highlight_color.set_green((u16::MAX / 5) * 4);

		let mut old_rect = Rectangle::new();
		for rect in &mut result_rects {
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
	}

	Ok(Some(RenderedContext {
		surface: ctx.target(),
		num_results,
		surface_width,
		surface_height
	}))
}

fn render_ctx_to_png(
	ctx: &RenderedContext,
	sender: &mut Sender<Result<RenderInfo, RenderError>>,
	(col_w, col_h): (u16, u16),
	page: usize
) -> Result<(), SendError<Result<RenderInfo, RenderError>>> {
	let mut img_data = Vec::with_capacity((ctx.surface_height * ctx.surface_width) as usize);

	match ctx.surface.write_to_png(&mut img_data) {
		Err(e) => sender.send(Err(RenderError::Render(format!(
			"Couldn't write surface to png: {e}"
		)))),
		Ok(()) => sender.send(Ok(RenderInfo::Page(PageInfo {
			img_data: ImageData {
				data: img_data,
				area: Rect {
					width: ctx.surface_width as u16 / col_w,
					height: ctx.surface_height as u16 / col_h,
					x: 0,
					y: 0
				}
			},
			page,
			search_results: ctx.num_results
		})))
	}
}
