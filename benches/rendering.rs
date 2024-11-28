mod utils;

use std::{
	hint::black_box,
	path::Path,
	time::{SystemTime, UNIX_EPOCH}
};

use criterion::{criterion_group, criterion_main, profiler::Profiler, BenchmarkId, Criterion};
use futures_util::StreamExt;
use tdf::{
	converter::{ConvertedPage, ConverterMsg},
	renderer::{fill_default, PageInfo, RenderInfo}
};
use utils::{
	handle_converter_msg, handle_renderer_msg, render_doc, start_all_rendering,
	start_converting_loop, start_rendering_loop, RenderState
};

const FILES: [&str; 3] = [
	"benches/adobe_example.pdf",
	"benches/example_dictionary.pdf",
	"benches/geotopo.pdf"
];

fn render_full(c: &mut Criterion) {
	for file in FILES {
		c.bench_with_input(BenchmarkId::new("render_full", file), &file, |b, &file| {
			b.to_async(tokio::runtime::Runtime::new().unwrap())
				.iter(|| render_doc(file))
		});
	}
}

fn render_to_first_page(c: &mut Criterion) {
	for file in FILES {
		c.bench_with_input(
			BenchmarkId::new("render_first_page", file),
			&file,
			|b, &file| {
				b.to_async(tokio::runtime::Runtime::new().unwrap())
					.iter(|| render_first_page(file))
			}
		);
	}
}

fn only_converting(c: &mut Criterion) {
	for file in FILES {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let all_rendered = runtime.block_on(render_all_files(file));

		c.bench_with_input(
			BenchmarkId::new("only_converting", file),
			&(all_rendered, file),
			|b, (rendered, _)| {
				b.to_async(tokio::runtime::Runtime::new().unwrap())
					.iter_with_setup(|| rendered.clone(), convert_all_files)
			}
		);
	}
}

pub async fn render_first_page(path: impl AsRef<Path>) {
	let RenderState {
		mut from_render_rx,
		mut from_converter_rx,
		mut pages,
		mut to_converter_tx,
		to_render_tx
	} = start_all_rendering(path);

	// we only want to render until the first page is ready to be printed
	while pages.iter().all(Option::is_none) {
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

async fn render_all_files(path: &'static str) -> Vec<PageInfo> {
	let (mut from_render_rx, to_render_tx) = start_rendering_loop(path);
	let mut pages = Vec::<Option<PageInfo>>::new();

	while let Some(info) = from_render_rx.next().await {
		match info.expect("Renderer ran into an error while rendering") {
			RenderInfo::NumPages(num) => fill_default(&mut pages, num),
			RenderInfo::Page(page) => {
				let num = page.page;
				pages[num] = Some(page);
			}
		};

		if pages.iter().all(Option::is_some) {
			break;
		}
	}

	drop(to_render_tx);
	pages.into_iter().flatten().collect()
}

async fn convert_all_files(files: Vec<PageInfo>) {
	let num_files = files.len();
	let (mut from_converter_rx, to_converter_tx) = start_converting_loop(num_files);

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

struct CpuProfiler;

impl Profiler for CpuProfiler {
	fn start_profiling(&mut self, benchmark_id: &str, _: &std::path::Path) {
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

criterion_group!(
	name = benches;
	config = Criterion::default().sample_size(40).with_profiler(CpuProfiler);
	targets = render_full, render_to_first_page, only_converting
);
criterion_main!(benches);
