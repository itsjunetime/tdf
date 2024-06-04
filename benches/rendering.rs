mod utils;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use utils::{render_doc, render_first_page};

const FILES: [&str; 2] = [
	"benches/example_dictionary.pdf",
	"benches/adobe_example.pdf"
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

criterion_group!(
	name = benches;
	config = Criterion::default().sample_size(40);
	targets = render_full, render_to_first_page
);
criterion_main!(benches);
