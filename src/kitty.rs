use std::io::Write;

use crossterm::{cursor::MoveTo, event::EventStream, execute};
use kittage::{
	AsyncInputReader, ImageDimensions, ImageId, PixelFormat,
	action::Action,
	delete::{ClearOrDelete, DeleteConfig, WhichToDelete},
	display::DisplayConfig,
	error::TransmitError,
	image::Image,
	medium::Medium
};
use ratatui::prelude::Rect;

use crate::converter::MaybeTransferred;

#[derive(Debug)]
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
			self.buf.clear();
		}
		self.w.flush()
	}
}

pub async fn run_action<'image, 'data, 'es>(
	action: Action<'image, 'data>,
	ev_stream: &'es mut EventStream
) -> Result<ImageId, TransmitError<<&'es mut EventStream as AsyncInputReader>::Error>>
{
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

pub async fn display_kitty_images(
	images: Vec<(usize, &mut MaybeTransferred, Rect)>,
	ev_stream: &mut EventStream
) -> Result<(), (Vec<usize>, String)> {
	if images.is_empty() {
		return Ok(());
	}

	run_action(
		Action::Delete(DeleteConfig {
			effect: ClearOrDelete::Clear,
			which: WhichToDelete::All
		}),
		ev_stream
	)
	.await
	.map_err(|e| (vec![], format!("Couldn't clear previous images: {e}")))?;

	let mut err = None;
	for (page_num, img, area) in images {
		let config = DisplayConfig::default();

		execute!(std::io::stdout(), MoveTo(area.x, area.y)).unwrap();

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
					Ok(img_id) => {
						*img = MaybeTransferred::Transferred(img_id);
						Ok(())
					}
					Err(e) => Err((Some(page_num), e.to_string())),
				}
			}
			MaybeTransferred::Transferred(image_id) => {
				run_action(
					Action::Display {
						image_id: *image_id,
						placement_id: *image_id,
						config
					},
					ev_stream
				)
				.await
				.map(|_| ())
				.map_err(|e| (None, e.to_string()))
			}
		};

		if let Err((id, e)) = this_err {
			let e = err.get_or_insert_with(|| (vec![], e));
			if let Some(id) = id {
				e.0.push(id);
			}
		}
	}

	match err {
		Some(e) => Err(e),
		None => Ok(())
	}
}
