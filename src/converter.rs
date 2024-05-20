use std::{pin::Pin, task::{Context, Poll}};

use futures_util::Stream;
use image::ImageFormat;
use itertools::Itertools;
use ratatui_image::{picker::Picker, protocol::Protocol, Resize};

use crate::renderer::{PageInfo, RenderError};

const MAX_ITER: usize = 20;

pub struct Converter {
	images: Vec<Option<PageInfo>>,
	picker: Picker,
	page: usize,
	// once it reaches 20, we're done rendering images
	iteration: usize
}

impl Converter {
	pub fn new(picker: Picker) -> Self {
		Self {
			images: vec![],
			picker,
			page: 0,
			iteration: 0
		}
	}

	pub fn add_img(&mut self, page: PageInfo) {
		let page_num = page.page;
		self.images[page_num] = Some(page);
		// just reset it to 0 so we grab this image again next time we try to get an image (if this
		// image is in the current list of iterations, so to speak)
		self.iteration = 0;
	}

	pub fn set_n_pages(&mut self, pages: usize) {
		self.images = Vec::with_capacity(pages);
		for _ in 0..pages {
			self.images.push(None);
		}

		self.page = self.page.min(pages - 1);
	}

	pub fn go_to_page(&mut self, page: usize) {
		self.page = page;
		self.iteration = 0;
	}

	pub fn change_page_by(&mut self, change: isize) {
		self.page = (self.page as isize + change) as usize;
		// We just reset iteration here. I think there's some heuristic we could do to place
		// iteration exactly where it needs to be to render the next page, but trying to determine
		// that caused me a lot of bugs, and only causes the slightest inefficiency (down below,
		// when we skip `self.iteration` elements in an iterator), so it's like whatever
		self.iteration = 0;
	}

	pub fn get_next_img(&mut self) -> Option<ConversionResult> {
		// In this fn, we return Poll::Pending and don't store a Waker 'cause this will be called
		// in a loop with tokio::select, and in no other context. The pending that we return on one
		// iteration will just be dropped/cancelled as soon as some other action happens, and then
		// next time select is called, this'll be checked again, and then we might be in the right
		// circumstance to return a Ready
		if self.iteration >= MAX_ITER || self.images.is_empty() {
			return None;
		}

		// This kinda mimics the way the renderer alternates between going above and below the
		// current page (within the bounds of how many pages there are) until we've done 20
		let idx_start = self.page.saturating_sub(MAX_ITER / 2);
		let idx_end = idx_start.saturating_add(MAX_ITER).min(self.images.len());

		// then we go through all the indices available to us and find the first one that has an
		// image available to steal
		let (page_info, iteration) = (idx_start..self.page)
			.interleave(self.page..idx_end)
			.enumerate()
			.skip(self.iteration)
			.find_map(|(i_idx, p_idx)|
				self.images[p_idx].take().map(|p| (p, i_idx))
			)?;

		let img_area = page_info.img_data.area;

		let dyn_img = match image::load_from_memory_with_format(&page_info.img_data.data, ImageFormat::Png) {
			Ok(dt) => dt,
			Err(e) => return Some(Err(RenderError::Render(format!("Couldn't convert Vec<u8> to DynamicImage: {e}"))))
		};

		// We don't actually want to Crop this image, but we've already
		// verified (with the ImageSurface stuff) that the image is the correct
		// size for the area given, so to save ratatui the work of having to
		// resize it, we tell them to crop it to fit.
		let txt_img = match self.picker.new_protocol(dyn_img, img_area, Resize::Crop) {
			Ok(img) => img,
			Err(e) => return Some(Err(RenderError::Render(format!("Couldn't convert DynamicImage to ratatui image: {e}"))))
		};

		// update the iteration to the iteration that we stole this image from
		self.iteration = iteration;

		Some(Ok(ConvertedPage {
			page: txt_img,
			num: page_info.page,
			num_results: page_info.search_results
		}))
	}
}

pub struct ConvertedPage {
	pub page: Box<dyn Protocol>,
	pub num: usize,
	pub num_results: usize
}

type ConversionResult = Result<ConvertedPage, RenderError>;

impl Stream for Converter {
	type Item = ConversionResult;

	fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
		match self.get_next_img() {
			Some(res) => Poll::Ready(Some(res)),
			None => Poll::Pending
		}
	}
}
