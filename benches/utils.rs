use std::{hint::black_box, path::Path};

use crossterm::terminal::WindowSize;
use flume::{unbounded, Sender};
use ratatui::layout::Rect;
use ratatui_image::picker::{Picker, ProtocolType};
use tdf::{converter::{run_conversion_loop, ConvertedPage, ConverterMsg}, renderer::{fill_default, start_rendering, RenderError, RenderInfo, RenderNotif}};
use futures_util::stream::StreamExt as _;

pub async fn render_doc(path: impl AsRef<Path>) {
	let pathbuf = path.as_ref().canonicalize().unwrap();
	let str_path = format!("file://{}", pathbuf.into_os_string().to_string_lossy());

	let (to_render_tx, from_main_rx) = unbounded();
	let (to_main_tx, from_render_rx) = unbounded();

	let font_size = (8, 14);
	let (columns, rows) = (60, 180);

	let size = WindowSize {
		columns,
		rows,
		height: rows * font_size.1,
		width: columns * font_size.0
	};

	std::thread::spawn(move || {
		start_rendering(str_path, to_main_tx, from_main_rx, size)
	});

	let (mut to_converter_tx, from_main_rx) = unbounded();
	let (to_main_tx, from_converter_rx) = unbounded();

	let mut picker = Picker::new(font_size);
	picker.protocol_type = ProtocolType::Kitty;

	tokio::spawn(run_conversion_loop(to_main_tx, from_main_rx, picker));

	let mut pages: Vec<Option<ConvertedPage>> = Vec::new();

	fn handle_renderer_msg(
		msg: Result<RenderInfo, RenderError>,
		pages: &mut Vec<Option<ConvertedPage>>,
		to_converter_tx: &mut Sender<tdf::converter::ConverterMsg>,
	) {
		match msg {
			Ok(RenderInfo::NumPages(num)) => {
				fill_default(pages, num);
				to_converter_tx.send(ConverterMsg::NumPages(num)).unwrap();
			},
			Ok(RenderInfo::Page(info)) => to_converter_tx.send(ConverterMsg::AddImg(info)).unwrap(),
			Err(e) => panic!("Got error from renderer: {e:?}")
		}
	}

	fn handle_converter_msg(
		msg: Result<ConvertedPage, RenderError>,
		pages: &mut [Option<ConvertedPage>],
		to_converter_tx: &mut Sender<ConverterMsg>
	) {
		let page = msg.expect("Got error from converter");
		let num = page.num;

		pages[num] = Some(page);

		let num_got = pages.iter()
			.filter(|p| p.is_some())
			.count();

		// we have to tell it to jump to a certain page so that it will actually render it (since
		// it only renders fanning out from the page that we currently have selected)
		to_converter_tx.send(ConverterMsg::GoToPage(num_got)).unwrap();
	}

	let main_area = Rect {
		x: 0,
		y: 0,
		width: columns - 2,
		height: rows - 6
	};
	to_render_tx.send(RenderNotif::Area(main_area)).unwrap();

	let mut from_render_rx = from_render_rx.into_stream();
	let mut from_converter_rx = from_converter_rx.into_stream();

	while pages.is_empty() || pages.iter().any(|p| p.is_none()) {
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
}
