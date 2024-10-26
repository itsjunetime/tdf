use std::{hint::black_box, path::Path};

use crossterm::terminal::WindowSize;
use flume::{r#async::RecvStream, unbounded, Sender};
use futures_util::stream::StreamExt as _;
use ratatui::layout::Rect;
use ratatui_image::picker::{Picker, ProtocolType};
use tdf::{
	converter::{run_conversion_loop, ConvertedPage, ConverterMsg},
	renderer::{fill_default, start_rendering, RenderError, RenderInfo, RenderNotif}
};

pub fn handle_renderer_msg(
	msg: Result<RenderInfo, RenderError>,
	pages: &mut Vec<Option<ConvertedPage>>,
	to_converter_tx: &mut Sender<tdf::converter::ConverterMsg>
) {
	match msg {
		Ok(RenderInfo::NumPages(num)) => {
			fill_default(pages, num);
			to_converter_tx.send(ConverterMsg::NumPages(num)).unwrap();
		}
		Ok(RenderInfo::Page(info)) => to_converter_tx.send(ConverterMsg::AddImg(info)).unwrap(),
		Err(e) => panic!("Got error from renderer: {e:?}")
	}
}

pub fn handle_converter_msg(
	msg: Result<ConvertedPage, RenderError>,
	pages: &mut [Option<ConvertedPage>],
	to_converter_tx: &mut Sender<ConverterMsg>
) {
	let page = msg.expect("Got error from converter");
	let num = page.num;

	pages[num] = Some(page);

	let num_got = pages.iter().filter(|p| p.is_some()).count();

	// we have to tell it to jump to a certain page so that it will actually render it (since
	// it only renders fanning out from the page that we currently have selected)
	to_converter_tx
		.send(ConverterMsg::GoToPage(num_got))
		.unwrap();
}

pub struct RenderState {
	pub from_render_rx: RecvStream<'static, Result<RenderInfo, RenderError>>,
	pub from_converter_rx: RecvStream<'static, Result<ConvertedPage, RenderError>>,
	pub pages: Vec<Option<ConvertedPage>>,
	pub to_converter_tx: Sender<ConverterMsg>,
	pub to_render_tx: Sender<RenderNotif>
}

const FONT_SIZE: (u16, u16) = (8, 14);

pub fn start_rendering_loop(
	path: impl AsRef<Path>
) -> (
	RecvStream<'static, Result<RenderInfo, RenderError>>,
	Sender<RenderNotif>
) {
	let pathbuf = path.as_ref().canonicalize().unwrap();
	let str_path = format!("file://{}", pathbuf.into_os_string().to_string_lossy());

	let (to_render_tx, from_main_rx) = unbounded();
	let (to_main_tx, from_render_rx) = unbounded();

	let (columns, rows) = (60, 180);

	let size = WindowSize {
		columns,
		rows,
		height: rows * FONT_SIZE.1,
		width: columns * FONT_SIZE.0
	};

	std::thread::spawn(move || start_rendering(&str_path, to_main_tx, from_main_rx, size));

	let main_area = Rect {
		x: 0,
		y: 0,
		width: columns - 2,
		height: rows - 6
	};
	to_render_tx.send(RenderNotif::Area(main_area)).unwrap();

	let from_render_rx = from_render_rx.into_stream();
	(from_render_rx, to_render_tx)
}

pub fn start_converting_loop(
	prerender: usize
) -> (
	RecvStream<'static, Result<ConvertedPage, RenderError>>,
	Sender<ConverterMsg>
) {
	let (to_converter_tx, from_main_rx) = unbounded();
	let (to_main_tx, from_converter_rx) = unbounded();

	let mut picker = Picker::new(FONT_SIZE);
	picker.protocol_type = ProtocolType::Kitty;

	tokio::spawn(run_conversion_loop(
		to_main_tx,
		from_main_rx,
		picker,
		prerender
	));

	let from_converter_rx = from_converter_rx.into_stream();
	(from_converter_rx, to_converter_tx)
}

pub fn start_all_rendering(path: impl AsRef<Path>) -> RenderState {
	let (from_render_rx, to_render_tx) = start_rendering_loop(path);
	let (from_converter_rx, to_converter_tx) = start_converting_loop(20);

	let pages: Vec<Option<ConvertedPage>> = Vec::new();

	RenderState {
		from_render_rx,
		from_converter_rx,
		pages,
		to_converter_tx,
		to_render_tx
	}
}

pub async fn render_doc(path: impl AsRef<Path>) {
	let RenderState {
		mut from_render_rx,
		mut from_converter_rx,
		mut pages,
		mut to_converter_tx,
		to_render_tx
	} = start_all_rendering(path);

	while pages.is_empty() || pages.iter().any(Option::is_none) {
		tokio::select! {
			Some(renderer_msg) = from_render_rx.next() => {
				handle_renderer_msg(renderer_msg, &mut pages, &mut to_converter_tx);
			},
			Some(converter_msg) = from_converter_rx.next() => {
				handle_converter_msg(converter_msg, &mut pages, &mut to_converter_tx);
			}
		}
	}

	black_box(pages);
	// we want to make sure this is kept around until the end of this function, or else the other
	// thread will see that this is disconnected and think that we're done communicating with them
	drop(to_render_tx);
}
