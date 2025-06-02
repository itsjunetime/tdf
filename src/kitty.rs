use std::io::Write;

use crossterm::event::EventStream;
use kittage::{AsyncInputReader, ImageId, action::Action, error::TransmitError};

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
			log::debug!("wrote {:?}", self.buf);
			self.buf.clear();
		}
		self.w.flush()
	}
}

pub async fn run_action<'image, 'data, 'es>(
	action: Action<'image, 'data>,
	ev_stream: &'es mut EventStream
) -> Result<ImageId, TransmitError<'image, 'data, <&'es mut EventStream as AsyncInputReader>::Error>>
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
