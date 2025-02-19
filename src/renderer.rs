use std::{thread::sleep, time::Duration};

use crossterm::terminal::WindowSize;
use flume::{Receiver, SendError, Sender, TryRecvError};
use itertools::Itertools;
use mupdf::{Colorspace, Document, Matrix, Page, Pixmap};
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
	Doc(mupdf::error::Error),
	Converting(String)
}

pub enum RenderInfo {
	NumPages(usize),
	Page(PageInfo),
	Reloaded
}

#[derive(Clone)]
pub struct PageInfo {
	pub img_data: ImageData,
	pub page_num: usize,
	pub result_rects: Vec<HighlightRect>
}

#[derive(Clone)]
pub struct ImageData {
	pub pixels: Vec<u8>,
	pub cell_area: Rect
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

// this function has to be sync (non-async) because the mupdf::Document needs to be held during
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
	sender: Sender<Result<RenderInfo, RenderError>>,
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

	let mut stored_doc = None;

	'reload: loop {
		let doc = match Document::open(path) {
			Err(e) => {
				// if there's an error, tell the main loop
				sender.send(Err(RenderError::Doc(e)))?;

				match stored_doc {
					Some(ref d) => d,
					None => {
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
				}
			}
			Ok(d) => {
				if stored_doc.is_some() {
					sender.send(Ok(RenderInfo::Reloaded))?;
				}
				&*stored_doc.insert(d)
			}
		};

		let n_pages = match doc.page_count() {
			Ok(n) => n as usize,
			Err(e) => {
				sender.send(Err(RenderError::Doc(e)))?;
				// just basic backoff i think
				sleep(Duration::from_secs(1));
				continue 'reload;
			}
		};

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

			let area_w = f32::from(area.width) * f32::from(col_w);
			let area_h = f32::from(area.height) * f32::from(col_h);

			// we go through each page
			for (num, rendered) in page_iter {
				// we only want to continue if one of the following is met:
				// 1. It failed to render last time (we want to retry)
				// 2. The `contained_term` is set to None (representing 'Unknown'), meaning that we
				//	  need to at least check if it contains the current term to see if it needs a
				//	  re-render
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
				let page = match doc.load_page(num as i32) {
					Err(e) => {
						sender.send(Err(RenderError::Doc(e)))?;
						continue;
					}
					Ok(p) => p
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
						rendered.contained_term = Some(ctx.result_rects.is_empty());
						rendered.successful = true;

						let cap = (ctx.pixmap.width()
							* ctx.pixmap.height() * u32::from(ctx.pixmap.n()))
							as usize;
						let mut pixels = Vec::with_capacity(cap);
						if let Err(e) = ctx.pixmap.write_to(&mut pixels, mupdf::ImageFormat::PNM) {
							sender.send(Err(RenderError::Doc(e)))?;
							continue;
						};

						sender.send(Ok(RenderInfo::Page(PageInfo {
							img_data: ImageData {
								pixels,
								cell_area: Rect {
									x: 0,
									y: 0,
									width: (ctx.surface_w / f32::from(col_w)) as u16,
									height: (ctx.surface_h / f32::from(col_h)) as u16
								}
							},
							page_num: num,
							result_rects: ctx.result_rects
						})))?;
					}
					// And if we got an error, then obviously we need to propagate that
					Err(e) => sender.send(Err(RenderError::Doc(e)))?
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
	pixmap: Pixmap,
	surface_w: f32,
	surface_h: f32,
	result_rects: Vec<HighlightRect>
}

fn render_single_page_to_ctx(
	page: &Page,
	search_term: Option<&str>,
	already_rendered_no_results: bool,
	(area_w, area_h): (f32, f32)
) -> Result<Option<RenderedContext>, mupdf::error::Error> {
	let mut max_hits = 10;
	let result_rects = loop {
		let rects = search_term
			.as_ref()
			// mupdf allocates a buffer of the size we give it to try to fill it with results. If we
			// pass in u32::MAX, it allocates too much memory to function. If we pass too small of a
			// number in, we may miss out on some of the results. Ideally, we'd like to make a better
			// interface than this, but we're stuck with this kinda ugly looping until we make sure
			// that we've found every instance of it on this page.
			.map(|term| page.search(term, max_hits))
			.transpose()?
			.unwrap_or_default();

		if rects.len() < (max_hits as usize) {
			break rects;
		}

		max_hits *= 2;
	};

	// If there are no search terms on this page, and we've already rendered it with no search
	// terms, then just return none to avoid this computation
	if result_rects.is_empty() && already_rendered_no_results {
		return Ok(None);
	}

	// then, get the size of the page
	let bounds = page.bounds()?;
	let (p_width, p_height) = (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);

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

	let surface_w = p_width * scale_factor;
	let surface_h = p_height * scale_factor;

	let colorspace = Colorspace::device_rgb();
	let matrix = Matrix::new_scale(scale_factor, scale_factor);

	let mut pixmap = page.to_pixmap(&matrix, &colorspace, 0.0, false)?;

	let (x_res, y_res) = pixmap.resolution();
	let new_x = (x_res as f32 * scale_factor) as i32;
	let new_y = (y_res as f32 * scale_factor) as i32;
	pixmap.set_resolution(new_x, new_y);

	let result_rects = result_rects
		.into_iter()
		.map(|quad| {
			let ul_x = (quad.ul.x * scale_factor) as u32;
			let ul_y = (quad.ul.y * scale_factor) as u32;
			let lr_x = (quad.lr.x * scale_factor) as u32;
			let lr_y = (quad.lr.y * scale_factor) as u32;
			HighlightRect {
				ul_x,
				ul_y,
				lr_x,
				lr_y
			}
		})
		.collect::<Vec<_>>();

	Ok(Some(RenderedContext {
		pixmap,
		surface_w,
		surface_h,
		result_rects
	}))
}

#[derive(Clone)]
pub struct HighlightRect {
	pub ul_x: u32,
	pub ul_y: u32,
	pub lr_x: u32,
	pub lr_y: u32
}
