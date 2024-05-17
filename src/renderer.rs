use cairo::{Antialias, Format};
use image::{DynamicImage, ImageFormat};
use itertools::Itertools;
use oxipng::Options;
use poppler::{Document, Page};
use ratatui::layout::Rect;
use tokio::sync::mpsc::{error::TryRecvError, Receiver, Sender};

pub enum RenderNotif {
	Area(Rect),
	JumpToPage(usize),
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
	Page(DynamicImage, usize)
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
	sender: Sender<Result<RenderInfo, RenderError>>,
	mut receiver: Receiver<RenderNotif>
) {
	// first, wait 'til we get told what the current starting area is so that we can set it to
	// know what to render to
	let mut area;
	loop {
		if let RenderNotif::Area(r) = receiver.blocking_recv().unwrap() {
			area = r;
			break;
		}
	};

	'reload: loop {
		let doc = match Document::from_file(&path, None) {
			Err(e) => {
				sender.blocking_send(Err(RenderError::Doc(e))).unwrap();
				return;
			},
			Ok(d) => d
		};

		let n_pages = doc.n_pages() as usize;
		sender.blocking_send(Ok(RenderInfo::NumPages(n_pages))).unwrap();

		// We're using this vec of bools to indicate which page numbers have already been rendered,
		// to support people jumping to specific pages and having quick rendering results. We
		// `split_at_mut` at 0 initially (which bascially makes `right == rendered && left == []`),
		// doing basically nothing, but if we get a notification that something has been jumped to,
		// then we can split at that page and render at both sides of it
		let mut rendered = vec![false; n_pages];
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
							let bigger = new_area.width > area.width || new_area.height > area.height;
							area = new_area;
							// we only want to re-render pages if the new area is greater than the old
							// one, 'cause then we might need sharper images to make it all look good.
							// If the new area is smaller, then the same high-quality-rendered images
							// will still look fine, so it's ok to leave it.
							if bigger {
								rendered = vec![false; n_pages];
								continue 'render_pages;
							}
						},
						RenderNotif::JumpToPage(page) => {
							start_point = page;
							continue 'render_pages;
						}
					}
				}
			}

			let (left, right) = rendered.split_at_mut(start_point);

			let page_iter = right.iter_mut()
				.enumerate()
				.map(|(idx, p)| (idx + start_point, p))
				.interleave(
					left.iter_mut()
						.enumerate()
						.map(|(idx, p)| (idx - (start_point + 1), p))
				);

			for (num, rendered) in page_iter {
				if *rendered {
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
				let page = doc.page(num as i32).unwrap();

				// render the page
				let to_send = render_single_page(page, area)
					.and_then(|img_data| match image::load_from_memory_with_format(&img_data, ImageFormat::Png) {
						Ok(img) => {
							// TODO find some way to do oxipng stuff maybe. Perchance throw them
							// all onto a new thread or whatever. idk.
							/*let sender_clone = sender.clone();
							std::thread::spawn(move || {
								let optimized = oxipng::optimize_from_memory(
									&img_data,
									&Options::default()
								).unwrap();
								let img = image::load_from_memory_with_format(&optimized, ImageFormat::Png).unwrap();
								sender_clone.blocking_send(Ok(RenderInfo::Page(img, num))).unwrap();
							});*/
							println!("data is {} while img is {}", img_data.len(), img.as_rgb8().unwrap().as_raw().len());
							Ok(img)
						},
						Err(e) => Err(format!("Couldn't create DynamicImage: {e}"))
					}).map(|img| RenderInfo::Page(img, num))
					.map_err(RenderError::Render);

				// then send it over
				sender.blocking_send(to_send).unwrap();

				*rendered = true;
			};
			// Then once we've rendered all these pages, wait until we get another notification
			// that this doc needs to be reloaded
			loop {
				// This once returned None despite the main thing being still connected (I think, at
				// last), so I'm just being safe here
				let Some(msg) = receiver.blocking_recv() else {
					return
				};
				handle_notif!(msg);
			}
		}
	}
}

fn render_single_page(
	page: Page,
	area: Rect,
//) -> Result<DynamicImage, String> {
) -> Result<Vec<u8>, String> {
	// First, get the font size; the number of pixels (width x height) per font character (I
	// think; it's at least something like that) on this terminal screen.
	let size = crossterm::terminal::window_size()
		.map_err(|e| format!("Couldn't get window size: {e}"))?;
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
		area_full_w as f64 / p_width
	} else {
		area_full_h as f64 / p_height
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
	).map_err(|e| format!("Couldn't create ImageSurface: {e}"))?;
	let ctx = cairo::Context::new(surface)
		.map_err(|e| format!("Couldn't create Context: {e}"))?;

	ctx.scale(scale_factor, scale_factor);

	// The default background color of PDFs (at least, I think) is white, so we need to set
	// that as the background color, then paint, then render.
	ctx.set_source_rgba(1.0, 1.0, 1.0, 1.0);
	ctx.set_antialias(Antialias::Best);
	ctx.paint().map_err(|e| format!("Couldn't paint Context: {e}"))?;
	page.render_for_printing(&ctx);
	ctx.scale(1. / scale_factor, 1. / scale_factor);

	let mut img_data = Vec::new();
	ctx.target().write_to_png(&mut img_data)
		.map_err(|e| format!("Couldn't write surface to png: {e}"))?;

	/*let img = image::load_from_memory_with_format(&img_data, ImageFormat::Png)
		.map_err(|e| format!("Couldn't load image from provided data: {e}"))?;

	Ok(img)*/
	Ok(img_data)
}
