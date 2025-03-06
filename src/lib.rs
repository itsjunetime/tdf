use std::num::NonZeroUsize;

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub enum PrerenderLimit {
	All,
	Limited(NonZeroUsize)
}

pub mod converter;
pub mod renderer;
pub mod skip;
pub mod tui;
