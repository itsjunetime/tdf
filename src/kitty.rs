use core::fmt::Display;
use std::{io::Write, num::NonZeroU32};

use crossterm::{
	cursor::MoveTo,
	event::EventStream,
	execute,
	terminal::{disable_raw_mode, enable_raw_mode}
};
use image::DynamicImage;
use kittage::{
	AsyncInputReader, ImageDimensions, ImageId, NumberOrId, PixelFormat,
	action::Action,
	delete::{ClearOrDelete, DeleteConfig, WhichToDelete},
	display::{CursorMovementPolicy, DisplayConfig, DisplayLocation},
	error::TransmitError,
	image::Image,
	medium::Medium
};
use ratatui::layout::Position;
use smallvec::SmallVec;

use crate::converter::MaybeTransferred;

pub struct KittyReadyToDisplay<'tui> {
	pub img: &'tui mut MaybeTransferred,
	pub page_num: usize,
	pub pos: Position,
	pub display_loc: DisplayLocation
}

pub enum KittyDisplay<'tui> {
	NoChange,
	ClearImages,
	DisplayImages(Vec<KittyReadyToDisplay<'tui>>)
}

pub struct DbgWriter<W: Write> {
	w: W,
	#[cfg(debug_assertions)]
	buf: String
}

impl<W: Write> Write for DbgWriter<W> {
	fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
		#[cfg(debug_assertions)]
		{
			if let Ok(s) = std::str::from_utf8(buf) {
				self.buf.push_str(s);
			}
		}
		self.w.write(buf)
	}

	fn flush(&mut self) -> std::io::Result<()> {
		#[cfg(debug_assertions)]
		{
			log::debug!("Writing to kitty: {:?}", self.buf);
			self.buf.clear();
		}
		self.w.flush()
	}
}

pub async fn run_action<'es>(
	action: Action<'_, '_>,
	ev_stream: &'es mut EventStream
) -> Result<Option<ImageId>, TransmitError<<&'es mut EventStream as AsyncInputReader>::Error>> {
	let writer = DbgWriter {
		w: std::io::stdout().lock(),
		#[cfg(debug_assertions)]
		buf: String::new()
	};
	action
		.execute_async(writer, ev_stream)
		.await
		.map(|(_, i)| i)
}

pub async fn do_shms_work(ev_stream: &mut EventStream) -> bool {
	let img = DynamicImage::new_rgb8(1, 1);
	let pid = std::process::id();
	let shm_name = format!("tdf_test_{pid}");

	#[cfg(unix)]
	let shm_name = &*shm_name;

	let Ok(mut k_img) = kittage::image::Image::shm_from(img, shm_name) else {
		return false;
	};

	// apparently the terminal won't respond to queries unless they have an Id instead of a number
	k_img.num_or_id = NumberOrId::Id(NonZeroU32::new(u32::MAX).unwrap());

	enable_raw_mode().unwrap();

	let res = run_action(Action::Query(&k_img), ev_stream).await;

	disable_raw_mode().unwrap();

	res.is_ok()
}

type ESTransErr<'es> = TransmitError<<&'es mut EventStream as AsyncInputReader>::Error>;

pub struct DisplayErr<'es> {
	pub failed_pages: SmallVec<[usize; 2]>,
	pub user_facing_err: &'static str,
	pub source: DisplayErrSource<'es>
}

impl<'es> DisplayErr<'es> {
	fn empty(user_facing_err: &'static str, source: ESTransErr<'es>) -> Self {
		Self {
			failed_pages: SmallVec::new(),
			user_facing_err,
			source: DisplayErrSource::Transmission(source)
		}
	}
}

#[derive(Debug)]
pub enum DisplayErrSource<'es> {
	KittageReturnedNoId,
	Transmission(ESTransErr<'es>)
}

impl Display for DisplayErrSource<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::KittageReturnedNoId => write!(f, "Kittage returned no ID when we asked it to display an image. This is a bug in kittage, please report it."),
			Self::Transmission(t) => write!(f, "Error with talking to the terminal: {t}"),
		}
	}
}

pub async fn display_kitty_images<'es>(
	display: KittyDisplay<'_>,
	ev_stream: &'es mut EventStream
) -> Result<(), DisplayErr<'es>> {
	let images = match display {
		KittyDisplay::NoChange => return Ok(()),
		KittyDisplay::DisplayImages(_) | KittyDisplay::ClearImages => {
			run_action(
				Action::Delete(DeleteConfig {
					effect: ClearOrDelete::Clear,
					which: WhichToDelete::All
				}),
				ev_stream
			)
			.await
			.map_err(|e| DisplayErr::empty("Couldn't clear previous images", e))?;

			let KittyDisplay::DisplayImages(images) = display else {
				return Ok(());
			};

			images
		}
	};

	let mut err = Ok::<(), (SmallVec<[usize; 2]>, DisplayErrSource<'es>)>(());
	for KittyReadyToDisplay {
		img,
		page_num,
		pos,
		display_loc
	} in images
	{
		let config = DisplayConfig {
			location: display_loc,
			cursor_movement: CursorMovementPolicy::DontMove,
			..DisplayConfig::default()
		};

		execute!(std::io::stdout(), MoveTo(pos.x, pos.y)).unwrap();

		log::debug!("going to display img {img:#?}");
		log::debug!("displaying with config {config:#?}");

		let this_err = match img {
			MaybeTransferred::NotYet(image) => {
				let mut fake_image = Image {
					num_or_id: image.num_or_id,
					format: PixelFormat::Rgb24(
						ImageDimensions {
							width: 0,
							height: 0
						},
						None
					),
					medium: Medium::Direct {
						chunk_size: None,
						data: (&[]).into()
					}
				};
				std::mem::swap(image, &mut fake_image);

				let res = run_action(
					Action::TransmitAndDisplay {
						image: fake_image,
						config,
						placement_id: None
					},
					ev_stream
				)
				.await;

				match res {
					Ok(Some(img_id)) => {
						*img = MaybeTransferred::Transferred(img_id);
						Ok(())
					},
					Ok(None) => Err((page_num, DisplayErrSource::KittageReturnedNoId)),
					Err(e) => Err((page_num, DisplayErrSource::Transmission(e)))
				}
			}
			MaybeTransferred::Transferred(image_id) => run_action(
				Action::Display {
					image_id: *image_id,
					placement_id: *image_id,
					config
				},
				ev_stream
			)
			.await
			// don't need the return id 'cause we already know it
			.map(|_: Option<ImageId>| ())
			.map_err(|e| (page_num, DisplayErrSource::Transmission(e)))
		};

		log::debug!("this_err is {this_err:#?}");

		if let Err((id, e)) = this_err {
			match err.as_mut() {
				Ok(()) => err = Err((SmallVec::from([id].as_slice()), e)),
				Err((v, _)) => v.push(id)
			}
		}
	}

	err.map_err(|(failed_pages, source)| DisplayErr {
		failed_pages,
		user_facing_err: "Couldn't transfer image to the terminal",
		source
	})
}
