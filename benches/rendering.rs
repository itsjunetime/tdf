mod utils;

use utils::render_doc;

use criterion::{criterion_group, criterion_main, Criterion};

fn render_dict(c: &mut Criterion) {
	c.bench_function(
		"example dictionary",
		|b| b.iter(||
			tokio::runtime::Runtime::new()
				.unwrap()
				.block_on(render_doc("./benches/example_dictionary.pdf"))
		)
	);
}

fn render_example(c: &mut Criterion) {
	c.bench_function(
		"adobe-provided sample",
		|b| b.iter(||
			tokio::runtime::Runtime::new()
				.unwrap()
				.block_on(render_doc("./benches/adobe_example.pdf"))
		)
	);
}

criterion_group!(benches, render_dict, render_example);
criterion_main!(benches);
