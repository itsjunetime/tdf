mod utils;

use std::{hint::black_box, path::Path};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use futures_util::StreamExt as _;
use ratatui_image::picker::ProtocolType;
use tdf::{
	converter::{ConvertedPage, ConverterMsg},
	renderer::{PageInfo, RenderInfo, fill_default}
};
use tokio::runtime::Runtime;
use utils::{
	RenderState, handle_converter_msg, handle_renderer_msg, render_doc, start_all_rendering,
	start_converting_loop, start_rendering_loop
};

const FILES: [&str; 3] = [
	"benches/adobe_example.pdf",
	"benches/example_dictionary.pdf",
	"benches/geotopo.pdf"
];

const PROTOS: [ProtocolType; 3] = [
	ProtocolType::Kitty,
	ProtocolType::Sixel,
	ProtocolType::Iterm2
];

const BLACK: i32 = 0;
const WHITE: i32 = i32::from_be_bytes([0, 0xff, 0xff, 0xff]);

fn for_all_combos(
	name: &'static str,
	mut f: impl FnMut(&Runtime, BenchmarkId, &'static str, ProtocolType)
) {
	let rt = tokio::runtime::Runtime::new().unwrap();
	for proto in PROTOS {
		for file in FILES {
			f(
				&rt,
				BenchmarkId::new(name, format!("{file},{proto:?}")),
				file,
				proto
			);
		}
	}
}

fn render_full(c: &mut Criterion) {
	for_all_combos("render_full", |rt, id, file, proto| {
		_ = c.bench_with_input(id, &file, |b, &file| {
			b.to_async(rt)
				.iter(|| render_doc(file, None, BLACK, WHITE, proto));
		});
	});
}

fn render_to_first_page(c: &mut Criterion) {
	for_all_combos("render_first_page", |rt, id, file, proto| {
		c.bench_with_input(id, &file, |b, &file| {
			b.to_async(rt)
				.iter(|| render_first_page(file, BLACK, WHITE, proto));
		});
	});
}

fn only_converting(c: &mut Criterion) {
	for_all_combos("only_converting", |rt, id, file, proto| {
		let all_rendered = rt.block_on(render_all_files(file, BLACK, WHITE));

		c.bench_with_input(id, &all_rendered, |b, rendered| {
			b.to_async(rt)
				.iter_with_setup(|| rendered.clone(), |f| convert_all_files(f, proto));
		});
	});
}

/*
fn search_short_common(c: &mut Criterion) {
	for_all_combos("search_short_common", |rt, id, file, proto| {
		c.bench_with_input(id, &file, |b, &file| {
			b.to_async(rt)
				.iter(|| render_doc(file, Some("an"), BLACK, WHITE, proto))
		});
	});
}

fn search_long_rare(c: &mut Criterion) {
	for_all_combos("search_long_rare", |rt, id, file, proto| {
		c.bench_with_input(id, &file, |b, &file| {
			b.to_async(rt)
				.iter(|| render_doc(file, Some("this is long and rare"), BLACK, WHITE, proto))
		});
	});
}
*/

pub async fn render_first_page(
	path: impl AsRef<Path>,
	black: i32,
	white: i32,
	proto: ProtocolType
) {
	let RenderState {
		mut from_render_rx,
		mut from_converter_rx,
		mut pages,
		to_converter_tx,
		to_render_tx
	} = start_all_rendering(path, black, white, proto);

	// we only want to render until the first page is ready to be printed
	while pages.iter().all(Option::is_none) {
		tokio::select! {
			Some(renderer_msg) = from_render_rx.next() => {
				handle_renderer_msg(renderer_msg, &mut pages, &to_converter_tx);
			},
			Some(converter_msg) = from_converter_rx.next() => {
				handle_converter_msg(converter_msg, &mut pages, &to_converter_tx);
			}
		}
	}

	black_box(pages);
	// we want to make sure this is kept around until the end of this function, or else the other
	// thread will see that this is disconnected and think that we're done communicating with them
	drop(to_render_tx);
}

async fn render_all_files(path: &'static str, black: i32, white: i32) -> Vec<PageInfo> {
	let (mut from_render_rx, to_render_tx) = start_rendering_loop(path, black, white);
	let mut pages = Vec::<Option<PageInfo>>::new();

	while let Some(info) = from_render_rx.next().await {
		match info.expect("Renderer ran into an error while rendering") {
			RenderInfo::Reloaded | RenderInfo::SearchResults { .. } => (),
			RenderInfo::NumPages(num) => fill_default(&mut pages, num),
			RenderInfo::Page(page) => {
				let num = page.page_num;
				pages[num] = Some(page);
			}
		}

		if pages.iter().all(Option::is_some) {
			break;
		}
	}

	drop(to_render_tx);
	pages.into_iter().flatten().collect()
}

async fn convert_all_files(files: Vec<PageInfo>, proto: ProtocolType) {
	let num_files = files.len();
	let (mut from_converter_rx, to_converter_tx) = start_converting_loop(proto, num_files);

	to_converter_tx
		.send(ConverterMsg::NumPages(num_files))
		.unwrap();

	let mut converted = Vec::<Option<ConvertedPage>>::new();
	fill_default(&mut converted, num_files);

	for page in files {
		to_converter_tx.send(ConverterMsg::AddImg(page)).unwrap();

		if !from_converter_rx.is_empty() {
			let page = from_converter_rx
				.next()
				.await
				.expect("Converter ended stream before expected")
				.expect("Converter ran into an error while converting page");

			let num = page.num;
			converted[num] = Some(page);
		}
	}

	while converted.iter().any(Option::is_none) {
		let page = from_converter_rx
			.next()
			.await
			.expect("Converted ended stream before expected")
			.expect("Converted ran into an error while converting page");

		let num = page.num;
		converted[num] = Some(page);
	}

	drop(to_converter_tx);
	black_box(converted);
}

/*
struct CpuProfiler;

impl criterion::profiler::Profiler for CpuProfiler {
	fn start_profiling(&mut self, benchmark_id: &str, _: &std::path::Path) {
		use std::time::{SystemTime, UNIX_EPOCH}
		let file = format!(
			"./{}-{}.profile",
			benchmark_id.replace('/', "-"),
			SystemTime::now()
				.duration_since(UNIX_EPOCH)
				.unwrap()
				.as_millis()
		);

		cpuprofiler::PROFILER.lock().unwrap().start(file).unwrap()
	}
	fn stop_profiling(&mut self, _: &str, _: &std::path::Path) {
		cpuprofiler::PROFILER.lock().unwrap().stop().unwrap();
	}
}
*/

criterion_group!(
	name = benches;
	// config = Criterion::default().sample_size(40).with_profiler(CpuProfiler);
	config = Criterion::default().sample_size(40);
	// targets = render_full, render_to_first_page, only_converting, search_short_common, search_long_rare
	targets = render_full, render_to_first_page, only_converting
);
criterion_main!(benches);
