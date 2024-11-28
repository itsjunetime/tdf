#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub mod converter;
pub mod renderer;
pub mod skip;
pub mod tui;
