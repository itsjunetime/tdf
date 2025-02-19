use flume::{Receiver, SendError, Sender, TryRecvError};
use futures_util::stream::StreamExt;
use image::DynamicImage;
use itertools::Itertools;
use ratatui_image::{picker::Picker, protocol::Protocol, Resize};
use rayon::iter::ParallelIterator;

use crate::renderer::{fill_default, PageInfo, RenderError};

pub struct ConvertedPage {
	pub page: Protocol,
	pub num: usize,
	pub num_results: usize
}

pub enum ConverterMsg {
	NumPages(usize),
	GoToPage(usize),
	AddImg(PageInfo)
}

pub async fn run_conversion_loop(
	sender: Sender<Result<ConvertedPage, RenderError>>,
	receiver: Receiver<ConverterMsg>,
	mut picker: Picker,
	prerender: usize
) -> Result<(), SendError<Result<ConvertedPage, RenderError>>> {
	let mut images = vec![];
	let mut page: usize = 0;

	fn next_page(
		images: &mut [Option<PageInfo>],
		picker: &mut Picker,
		page: usize,
		iteration: &mut usize,
		prerender: usize
	) -> Result<Option<ConvertedPage>, RenderError> {
		if images.is_empty() || *iteration >= prerender {
			return Ok(None);
		}

		// This kinda mimics the way the renderer alternates between going above and below the
		// current page (within the bounds of how many pages there are) until we've done 20
		let idx_start = page.saturating_sub(prerender / 2);
		let idx_end = idx_start.saturating_add(prerender).min(images.len());

		// then we go through all the indices available to us and find the first one that has an
		// image available to steal
		let Some((page_info, new_iter)) = (idx_start..page)
			.interleave(page..idx_end)
			.enumerate()
			.skip(*iteration)
			.find_map(|(i_idx, p_idx)| images[p_idx].take().map(|p| (p, i_idx)))
		else {
			return Ok(None);
		};

		let mut dyn_img = image::load_from_memory_with_format(
			&page_info.img_data.pixels,
			image::ImageFormat::Pnm
		)
		.map_err(|e| RenderError::Converting(format!("Can't load image: {e}")))?;

		match dyn_img {
			DynamicImage::ImageRgb8(ref mut img) =>
				for quad in &*page_info.result_rects {
					img.par_enumerate_pixels_mut()
						.filter(|(x, y, _)| {
							*x > quad.ul_x && *x < quad.lr_x && *y > quad.ul_y && *y < quad.lr_y
						})
						.for_each(|(_, _, px)| px.0[2] = px.0[2].saturating_sub(u8::MAX / 2));
				},
			_ => unreachable!()
		};

		let img_area = page_info.img_data.cell_area;

		// We don't actually want to Crop this image, but we've already
		// verified (with the ImageSurface stuff) that the image is the correct
		// size for the area given, so to save ratatui the work of having to
		// resize it, we tell them to crop it to fit.
		let txt_img = picker
			.new_protocol(dyn_img, img_area, Resize::None)
			.map_err(|e| {
				RenderError::Converting(format!(
					"Couldn't convert DynamicImage to ratatui image: {e}"
				))
			})?;

		// update the iteration to the iteration that we stole this image from
		*iteration = new_iter;

		Ok(Some(ConvertedPage {
			page: txt_img,
			num: page_info.page_num,
			num_results: page_info.result_rects.len()
		}))
	}

	fn handle_notif(msg: ConverterMsg, images: &mut Vec<Option<PageInfo>>, page: &mut usize) {
		match msg {
			ConverterMsg::AddImg(img) => {
				let page_num = img.page_num;
				images[page_num] = Some(img);
			}
			ConverterMsg::NumPages(n_pages) => {
				fill_default(images, n_pages);
				*page = (*page).min(n_pages - 1);
			}
			ConverterMsg::GoToPage(new_page) => *page = new_page
		}
	}

	'outer: loop {
		let mut iteration = 0;
		loop {
			match receiver.try_recv() {
				Ok(msg) => {
					handle_notif(msg, &mut images, &mut page);
					continue 'outer;
				}
				Err(TryRecvError::Empty) => (),
				// if it's disconnected, we're done. just return.
				Err(TryRecvError::Disconnected) => return Ok(())
			}

			match next_page(&mut images, &mut picker, page, &mut iteration, prerender) {
				Ok(None) => break,
				Ok(Some(img)) => sender.send(Ok(img))?,
				Err(e) => sender.send(Err(e))?
			}
		}

		let Some(msg) = receiver.stream().next().await else {
			break;
		};

		handle_notif(msg, &mut images, &mut page);
	}

	Ok(())
}
