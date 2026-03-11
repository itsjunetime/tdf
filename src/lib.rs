use std::num::NonZeroUsize;

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(PartialEq)]
pub enum PrerenderLimit {
	All,
	Limited(NonZeroUsize)
}

pub mod converter;
pub mod kitty;
pub mod renderer;
pub mod skip;
pub mod tui;

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum FitOrFill {
	Fit,
	Fill
}

pub struct ScaledResult {
	width: f32,
	height: f32,
	scale_factor: f32
}

#[must_use]
pub fn scale_img_for_area(
	(img_width, img_height): (f32, f32),
	(area_width, area_height): (f32, f32),
	fit_or_fill: FitOrFill
) -> ScaledResult {
	// and get its aspect ratio
	let img_aspect_ratio = img_width / img_height;

	// Then we get the full pixel dimensions of the area provided to us, and the aspect ratio
	// of that area
	let area_aspect_ratio = area_width / area_height;

	// and get the ratio that this page would have to be scaled by to fit perfectly within the
	// area provided to us.
	// we do this first by comparing the aspect ratio of the page with the aspect ratio of the
	// area to fit it within. If the aspect ratio of the page is larger, then we need to scale
	// the width of the page to fill perfectly within the height of the area. Otherwise, we
	// scale the height to fit perfectly. The dimension that _is not_ scaled to fit perfectly
	// is scaled by the same factor as the dimension that _is_ scaled perfectly.
	let scale_factor = match (img_aspect_ratio > area_aspect_ratio, fit_or_fill) {
		(true, FitOrFill::Fit) | (false, FitOrFill::Fill) => area_width / img_width,
		(false, FitOrFill::Fit) | (true, FitOrFill::Fill) => area_height / img_height
	};

	ScaledResult {
		width: img_width * scale_factor,
		height: img_height * scale_factor,
		scale_factor
	}
}
