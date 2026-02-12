use std::{collections::VecDeque, num::NonZeroUsize, path::Path, thread::sleep, time::Duration};

use flume::{Receiver, SendError, Sender, TryRecvError};
use mupdf::{
	Colorspace, Document, Matrix, Page, Pixmap, Quad, TextPageFlags, text_page::SearchHitResponse
};
use ratatui::layout::Rect;

use crate::{
	FitOrFill, PrerenderLimit, ScaledResult, scale_img_for_area, skip::InterleavedAroundWithMax
};

const KITTY_MAX_W_OR_H: f32 = 10_000.0;

#[derive(Debug)]
pub enum RenderNotif {
	Area(Rect),
	JumpToPage(usize),
	PageNeedsReRender(usize),
	Search(String),
	SwitchFitOrFill(FitOrFill),
	Reload,
	Invert,
	Rotate
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
	SearchResults { page_num: usize, num_results: usize },
	Reloaded
}

#[derive(Debug, Copy, Clone)]
pub enum RotateDirection {
	Deg0,
	Deg90,
	Deg180,
	Deg270
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
	pub cell_w: u16,
	pub cell_h: u16
}

#[derive(Default)]
struct PrevRender {
	successful: bool,
	num_search_found: Option<usize>
}

pub const MUPDF_BLACK: i32 = 0;
pub const MUPDF_WHITE: i32 = i32::from_be_bytes([0, 0xff, 0xff, 0xff]);

#[inline]
pub fn fill_default<T: Default>(vec: &mut Vec<T>, size: usize) {
	vec.clear();
	vec.resize_with(size, T::default);
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
#[expect(clippy::needless_pass_by_value, clippy::too_many_arguments)]
pub fn start_rendering(
	path: &Path,
	sender: Sender<Result<RenderInfo, RenderError>>,
	receiver: Receiver<RenderNotif>,
	col_h: u16,
	col_w: u16,
	prerender: PrerenderLimit,
	black: i32,
	white: i32
) -> Result<(), SendError<Result<RenderInfo, RenderError>>> {
	// We want this outside of 'reload so that if the doc reloads, the search term that somebody
	// set will still get highlighted in the reloaded doc
	let mut search_term = None;

	// And although the font size could theoretically change, we aren't accounting for that right
	// now, so we just use the values passed in.

	let mut stored_doc = None;
	let mut invert = false;
	let mut rotate = RotateDirection::Deg0;
	let mut preserved_area = None;
	let mut fit_or_fill = FitOrFill::Fit;

	let mut need_rerender = VecDeque::new();

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
							if matches!(msg, RenderNotif::Reload) {
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
			Ok(n) => match NonZeroUsize::new(n as usize) {
				Some(n) => n,
				None => {
					sleep(Duration::from_secs(1));
					continue 'reload;
				}
			},
			Err(e) => {
				sender.send(Err(RenderError::Doc(e)))?;
				// just basic backoff i think
				sleep(Duration::from_secs(1));
				continue 'reload;
			}
		};

		sender.send(Ok(RenderInfo::NumPages(n_pages.get())))?;

		// We're using this vec to indicate which page numbers have already been rendered, to
		// support people jumping to specific pages and having quick rendering results. We
		// `split_at_mut` at 0 initially (which bascially makes `right == rendered && left == []`),
		// doing basically nothing, but if we get a notification that something has been jumped to,
		// then we can split at that page and render at both sides of it
		let mut rendered = Vec::new();
		fill_default::<PrevRender>(&mut rendered, n_pages.get());
		let mut start_point = 0;

		// This is kinda a weird way of doing this, but if we get a notification that the area
		// changed, we want to start re-rending all of the pages, but we don't want to reload the
		// document. If there was a mechanism to say 'start this for-loop over' then I would do
		// that, but I don't think such a thing exists, so this is our attempt
		'render_pages: loop {
			// next, we gotta wait 'til we get told what the current starting area is so that we can
			// set it to know what to render to
			let area = preserved_area.unwrap_or_else(|| {
				let new_area = loop {
					if let RenderNotif::Area(r) = receiver.recv().unwrap() {
						break r;
					}
				};
				preserved_area = Some(new_area);
				new_area
			});

			let area_w = f32::from(area.width) * f32::from(col_w);
			let area_h = f32::from(area.height) * f32::from(col_h);

			// what we do with a notif is the same regardless of if we're in the middle of
			// rendering the list of pages or we're all done
			macro_rules! handle_notif {
				($notif:ident) => {{
					match $notif {
						RenderNotif::Reload => continue 'reload,
						RenderNotif::Invert => {
							invert = !invert;
							for page in &mut rendered {
								page.successful = false;
							}
							continue 'render_pages;
						}
						RenderNotif::Area(new_area) => {
							preserved_area = Some(new_area);
							fill_default(&mut rendered, n_pages.get());
							continue 'render_pages;
						}
						RenderNotif::SwitchFitOrFill(f_or_f) =>
							if f_or_f != fit_or_fill {
								fit_or_fill = f_or_f;
								fill_default(&mut rendered, n_pages.get());
								continue 'render_pages;
							},
						RenderNotif::JumpToPage(page) => {
							start_point = page;
							continue 'render_pages;
						}
						RenderNotif::PageNeedsReRender(page) => {
							rendered[page].successful = false;
							need_rerender.push_back(page);
							continue 'render_pages;
						}
						RenderNotif::Search(term) => {
							if term.is_empty() {
								// If the term is set to nothing, then we don't need to re-render
								// the pages wherein there were already no search results. So this
								// is a little optimization to allow that.
								for page in &mut rendered {
									if page.num_search_found.is_some_and(|n| n > 0) {
										page.num_search_found = Some(0);
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
									page.num_search_found = None;
								}
								search_term = Some(term);
							}
							continue 'render_pages;
						}
						RenderNotif::Rotate => {
							rotate = match rotate {
								RotateDirection::Deg0 => RotateDirection::Deg90,
								RotateDirection::Deg90 => RotateDirection::Deg180,
								RotateDirection::Deg180 => RotateDirection::Deg270,
								RotateDirection::Deg270 => RotateDirection::Deg0
							};
							for page in &mut rendered {
								page.successful = false;
							}
							continue 'render_pages;
						}
					}
				}};
			}

			let any_not_searched = rendered.iter().any(|r| r.num_search_found.is_none());

			// This is our iterator over all the pages we want to look at and render. It uses this
			// weird 'interleave' thing to render pages on *both sides* of the currently-displayed
			// page in case they device to go forward or backwards.
			let page_iter = PopOnNext {
				inner: &mut need_rerender
			}
			.chain(InterleavedAroundWithMax::new(start_point, 0, n_pages).take(
				match (&prerender, &search_term) {
					// If the user has limited the amount of pages they want to prerender, then we
					// just do what they ask. Nice and easy.
					(PrerenderLimit::Limited(l), _) => l.get(),
					// If they haven't limited it, but we don't have any search term that we're
					// currently looking for, just go for all of it
					(PrerenderLimit::All, None) => n_pages.get(),
					// If they haven't limited it, and we DO have a search term we need to look
					// for, just do 20 so that we don't dramatically slow down the search process
					// since they've specifically initiated that and so we want it to take priority
					(PrerenderLimit::All, Some(_)) =>
						if any_not_searched {
							20
						} else {
							n_pages.get()
						},
				}
			));

			// we go through each page
			for page_num in page_iter {
				let rendered = &mut rendered[page_num];

				// we only want to continue if one of the following is met:
				// 1. It failed to render last time (we want to retry)
				// 2. The `contained_term` is set to Unknown, meaning that we need to at least
				//    check if it contains the current term to see if it needs a re-render
				if rendered.successful && rendered.num_search_found.is_some() {
					continue;
				}

				// We know this is in range 'cause we're iterating over it but we still just want
				// to be safe
				let page = match doc.load_page(page_num as i32) {
					Err(e) => {
						sender.send(Err(RenderError::Doc(e)))?;
						continue;
					}
					Ok(p) => p
				};

				// render the page
				match render_single_page_to_ctx(
					&page,
					search_term.as_deref(),
					rendered,
					invert,
					black,
					white,
					fit_or_fill,
					rotate,
					(area_w, area_h)
				) {
					// If that fn returned Some, that means it needed to be re-rendered for some
					// reason or another, so we're sending it here
					Ok(ctx) => {
						let w = ctx.pixmap.width();
						let h = ctx.pixmap.height();
						let cap = (w * h * u32::from(ctx.pixmap.n())) as usize + 16;
						let mut pixels = Vec::with_capacity(cap);
						if let Err(e) = ctx.pixmap.write_to(&mut pixels, mupdf::ImageFormat::PNM) {
							sender.send(Err(RenderError::Doc(e)))?;
							continue;
						}

						log::debug!("got pixmap for page {page_num} with WxH {w}x{h}");

						rendered.num_search_found = Some(ctx.result_rects.len());
						rendered.successful = true;

						sender.send(Ok(RenderInfo::Page(PageInfo {
							img_data: ImageData {
								pixels,
								cell_w: (ctx.surface_w / f32::from(col_w)) as u16,
								cell_h: (ctx.surface_h / f32::from(col_h)) as u16
							},
							page_num,
							result_rects: ctx.result_rects
						})))?;
					}
					// And if we got an error, then obviously we need to propagate that
					Err(e) => sender.send(Err(RenderError::Doc(e)))?
				}

				// check if we've been told to change the area that we're rendering to,
				// or if we're told to rerender
				match receiver.try_recv() {
					// If it's disconnected, then the main loop is done, so we should just give up
					Err(TryRecvError::Disconnected) => return Ok(()),
					Ok(notif) => handle_notif!(notif),
					Err(TryRecvError::Empty) => ()
				}
			}

			// Now, if we have a search term, we want to look through the rest of the document past
			// what we've just rendered (and looked at the search results of)
			if let Some(ref term) = search_term {
				let mut search_start = start_point;
				loop {
					// hmm maybe this would be nice to make configurable but whatever
					const SEARCH_AT_TIME: usize = 20;

					// So now we want to look through all the remaining pages, starting after this
					// current one (we don't do interleaving here 'cause I'm lazy
					let page_idx = rendered[search_start..]
						.iter_mut()
						.enumerate()
						// And we only want to take max SEARCH_AT_TIME of them since we don't want
						// to block on this for *too* long
						.take(SEARCH_AT_TIME)
						// And we only want the ones that we still don't know about...
						.filter(|(_, r)| r.num_search_found.is_none())
						// And then adjust the index to be correct for the actual page number
						.map(|(idx, r)| (idx + search_start, r));

					// then we go through each...
					for (page_num, rendered) in page_idx {
						// We get the number of results (using the function that specifically just
						// counts them instead of determining the quads of them all)
						let num_results = doc
							.load_page(page_num as i32)
							.and_then(|page| count_search_results(&page, term))
							.unwrap();

						// And mark that whatever else was rendered last is not relevant anymore if
						// there are results that need to be rendered
						if num_results > 0 {
							rendered.successful = false;
						}
						// Mark the `contained_term` field with this updated value...
						rendered.num_search_found = Some(num_results);

						// And send it over to the tui so that they can know and use it to
						// determine what next page to jump to
						sender.send(Ok(RenderInfo::SearchResults {
							page_num,
							num_results
						}))?;
					}

					// then once we're done with this iteration, we increment search_start to
					// prepare for the next iteration
					search_start += SEARCH_AT_TIME;

					// now, we want to check if we've gone past the end - if so, we go back to the
					// beginning so we can get the pages before the current one.
					if search_start > n_pages.get() {
						if start_point == 0 {
							break;
						}

						search_start = 0;
					} else if ((search_start - SEARCH_AT_TIME) + 1..search_start)
						.contains(&start_point)
					{
						// And if we are back at the place we started, we've looked through all the
						// pages. Quit.
						break;
					}

					match receiver.try_recv() {
						// If there are no messages left for us, just continue in this loop
						Err(TryRecvError::Empty) => (),
						Err(TryRecvError::Disconnected) => return Ok(()),
						Ok(msg) => handle_notif!(msg)
					}
				}
			}

			// So now we've just *searched* all the pages but not necessarily rendered all of them.
			// So if there are any we have yet to render, we need to loop back to the beginning of
			// this loop to continue rendering all of them
			if rendered.iter().any(|r| !r.successful) && prerender == PrerenderLimit::All {
				continue;
			}

			// Then once we've rendered all these pages, wait until we get another notification
			// that this doc needs to be reloaded
			// This once returned None despite the main thing being still connected (I think, at
			// least), so I'm just being safe here
			let Ok(msg) = receiver.recv() else {
				return Ok(());
			};

			handle_notif!(msg);
		}
	}
}

struct RenderedContext {
	pixmap: Pixmap,
	surface_w: f32,
	surface_h: f32,
	result_rects: Vec<HighlightRect>
}

#[expect(clippy::too_many_arguments)]
fn render_single_page_to_ctx(
	page: &Page,
	search_term: Option<&str>,
	prev_render: &PrevRender,
	invert: bool,
	black: i32,
	white: i32,
	fit_or_fill: FitOrFill,
	rotate: RotateDirection,
	(area_w, area_h): (f32, f32)
) -> Result<RenderedContext, mupdf::error::Error> {
	let result_rects = match prev_render.num_search_found {
		None => search_page(page, search_term, 0)?,
		Some(0) => Vec::new(),
		Some(count @ 1..) => search_page(page, search_term, count)?
	};

	// then, get the size of the page
	let bounds = page.bounds()?;
	let page_dim = match rotate {
		RotateDirection::Deg0 | RotateDirection::Deg180 =>
			(bounds.x1 - bounds.x0, bounds.y1 - bounds.y0),
		RotateDirection::Deg90 | RotateDirection::Deg270 =>
			(bounds.y1 - bounds.y0, bounds.x1 - bounds.x0),
	};

	let scaled = scale_img_for_area(page_dim, (area_w, area_h), fit_or_fill);
	let ScaledResult {
		width: mut surface_w,
		height: mut surface_h,
		mut scale_factor
	} = scaled;

	if surface_w > KITTY_MAX_W_OR_H || surface_h > KITTY_MAX_W_OR_H {
		let descale = (surface_w / KITTY_MAX_W_OR_H).max(surface_h / KITTY_MAX_W_OR_H);
		surface_w /= descale;
		surface_h /= descale;
		scale_factor /= descale;
	}

	let colorspace = Colorspace::device_rgb();
	let mut matrix = Matrix::new_scale(scale_factor, scale_factor);
	match rotate {
		RotateDirection::Deg0 => matrix.rotate(0.0),
		RotateDirection::Deg90 => matrix.rotate(90.0),
		RotateDirection::Deg180 => matrix.rotate(180.0),
		RotateDirection::Deg270 => matrix.rotate(270.0)
	};

	let mut pixmap = page.to_pixmap(&matrix, &colorspace, false, false)?;
	if invert {
		pixmap.tint(white, black)?;
	} else if black != MUPDF_BLACK || white != MUPDF_WHITE {
		pixmap.tint(black, white)?;
	}

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

	Ok(RenderedContext {
		pixmap,
		surface_w,
		surface_h,
		result_rects
	})
}

#[derive(Clone, Debug)]
pub struct HighlightRect {
	pub ul_x: u32,
	pub ul_y: u32,
	pub lr_x: u32,
	pub lr_y: u32
}

#[inline]
fn search_page(
	page: &Page,
	search_term: Option<&str>,
	trusted_search_results: usize
) -> Result<Vec<Quad>, mupdf::error::Error> {
	search_term
		.map(|term| {
			page.to_text_page(TextPageFlags::empty()).and_then(|page| {
				let mut v = Vec::with_capacity(trusted_search_results);
				page.search_cb(term, &mut v, |v, results| {
					v.extend(results.iter().cloned());
					SearchHitResponse::ContinueSearch
				})
				.map(|_| v)
			})
		})
		.transpose()
		.map(Option::unwrap_or_default)
}

#[inline]
fn count_search_results(page: &Page, search_term: &str) -> Result<usize, mupdf::error::Error> {
	page.to_text_page(TextPageFlags::empty()).and_then(|page| {
		let mut count = 0;
		page.search_cb(search_term, &mut count, |count, results| {
			*count += results.len();
			SearchHitResponse::ContinueSearch
		})?;
		Ok(count)
	})
}

struct PopOnNext<'a> {
	inner: &'a mut VecDeque<usize>
}

impl Iterator for PopOnNext<'_> {
	type Item = usize;
	fn next(&mut self) -> Option<Self::Item> {
		self.inner.pop_front()
	}

	fn size_hint(&self) -> (usize, Option<usize>) {
		let l = self.len();
		(l, Some(l))
	}
}

impl ExactSizeIterator for PopOnNext<'_> {
	fn len(&self) -> usize {
		self.inner.len()
	}
}
