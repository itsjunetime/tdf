#![feature(if_let_guard)]

use std::{io::stdout, path::PathBuf, str::FromStr};

use converter::{run_conversion_loop, ConvertedPage, ConverterMsg};
use crossterm::{
	execute,
	terminal::{
		disable_raw_mode, enable_raw_mode, window_size, EndSynchronizedUpdate,
		EnterAlternateScreen, LeaveAlternateScreen
	}
};
use futures_util::{stream::StreamExt, FutureExt};
use glib::{LogField, LogLevel, LogWriterOutput};
use notify::{RecursiveMode, Watcher};
use ratatui::{backend::CrosstermBackend, Terminal};
use ratatui_image::picker::Picker;
use renderer::{RenderInfo, RenderNotif};
use tui::{InputAction, Tui};

mod converter;
mod renderer;
mod skip;
mod tui;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
	let mut args = std::env::args().skip(1);
	let file = args.next().ok_or("Program requires a file to process")?;
	let path = PathBuf::from_str(&file)?.canonicalize()?;

	let (watch_tx, render_rx) = tokio::sync::mpsc::unbounded_channel();
	let tui_tx = watch_tx.clone();

	// we need to call this outside the recommended_watcher call because if we call it inside, that
	// will be calling it from a thread not owned by the tokio runtime (since it's created by
	// calling thread::spawn) and that will cause a panic
	let mut watcher = notify::recommended_watcher(move |_| {
		// This shouldn't fail to send unless the receiver gets disconnected. If that's happened,
		// then like the main thread has panicked or something, so it doesn't matter if this panics
		// as well
		watch_tx.send(renderer::RenderNotif::Reload).unwrap();
	})?;

	// We're making this nonrecursive 'cause we're just watching a single file, so there's nothing
	// to recurse into
	watcher.watch(&path, RecursiveMode::NonRecursive)?;

	let file_path = format!("file://{}", path.clone().into_os_string().to_string_lossy());
	let (render_tx, mut tui_rx) = tokio::sync::mpsc::unbounded_channel();

	let window_size = window_size()?;

	// We need to create `picker` on this thread because if we create it on the `renderer` thread,
	// it messes up something with user input. Input never makes it to the crossterm thing
	let mut picker = Picker::new((
		window_size.width / window_size.columns,
		window_size.height / window_size.rows
	));
	picker.guess_protocol();

	// then we want to spawn off the rendering task
	// We need to use the thread::spawn API so that this exists in a thread not owned by tokio,
	// since the methods we call in `start_rendering` will panic if called in an async context
	std::thread::spawn(move || {
		renderer::start_rendering(file_path, render_tx, render_rx, window_size)
	});

	let mut ev_stream = crossterm::event::EventStream::new();

	let (to_converter, from_main) = tokio::sync::mpsc::unbounded_channel();
	let (to_main, mut from_converter) = tokio::sync::mpsc::unbounded_channel();

	tokio::spawn(run_conversion_loop(to_main, from_main, picker));

	let file_name = path
		.file_name()
		.map(|n| n.to_string_lossy())
		.unwrap_or_else(|| "Unknown file".into())
		.to_string();
	let mut tui = tui::Tui::new(file_name);

	let backend = CrosstermBackend::new(std::io::stdout());
	let mut term = Terminal::new(backend)?;
	term.skip_diff(true);

	// poppler has some annoying logging (e.g. if you request a page index out-of-bounds of a
	// document's pages, then it will return `None`, but still log to stderr with CRITICAL level),
	// so we want to just ignore all logging since this is a tui app.
	glib::log_set_writer_func(noop);

	execute!(
		term.backend_mut(),
		EnterAlternateScreen,
		crossterm::cursor::Hide
	)?;
	enable_raw_mode()?;

	let mut main_area = tui::Tui::main_layout(&term.get_frame());
	tui_tx.send(RenderNotif::Area(main_area[1]))?;

	loop {
		let mut needs_redraw = tokio::select! {
			// First we check if we have any keystrokes
			Some(ev) = ev_stream.next().fuse() => {
				// If we can't get user input, just crash.
				let ev = ev.expect("Couldn't get any user input");

				match tui.handle_event(ev) {
					None => false,
					Some(action) => {
						match action {
							InputAction::Redraw => (),
							InputAction::QuitApp => break,
							InputAction::JumpingToPage(page) => {
								tui_tx.send(RenderNotif::JumpToPage(page))?;
								to_converter.send(ConverterMsg::GoToPage(page))?;
							},
							InputAction::Search(term) => tui_tx.send(RenderNotif::Search(term))?,
						};
						true
					}
				}
			},
			Some(renderer_msg) = tui_rx.recv() => {
				match renderer_msg {
					Ok(RenderInfo::NumPages(num)) => {
						tui.set_n_pages(num);
						to_converter.send(ConverterMsg::NumPages(num))?;
					},
					Ok(RenderInfo::Page(info)) => {
						tui.got_num_results_on_page(info.page, info.search_results);
						to_converter.send(ConverterMsg::AddImg(info))?;
					},
					Err(e) => tui.show_error(e),
				}
				true
			}
			Some(img_res) = from_converter.recv() => {
				match img_res {
					Ok(ConvertedPage { page, num, num_results }) => tui.page_ready(page, num, num_results),
					Err(e) => tui.show_error(e),
				}
				true
			},
		};

		let new_area = Tui::main_layout(&term.get_frame());
		if new_area != main_area {
			main_area = new_area;
			tui_tx.send(RenderNotif::Area(main_area[1]))?;
			needs_redraw = true;
		}

		if needs_redraw {
			term.draw(|f| {
				tui.render(f, &main_area);
			})?;
			execute!(stdout(), EndSynchronizedUpdate)?;
		}
	}

	execute!(
		term.backend_mut(),
		LeaveAlternateScreen,
		crossterm::cursor::Show
	)?;
	disable_raw_mode()?;

	Ok(())
}

fn noop(_: LogLevel, _: &[LogField<'_>]) -> LogWriterOutput {
	LogWriterOutput::Handled
}
