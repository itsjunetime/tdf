use std::{path::PathBuf, str::FromStr};

use converter::Converter;
use crossterm::{execute, terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}};
use glib::{LogField, LogLevel, LogWriterOutput};
use notify::{RecursiveMode, Watcher};
use ratatui::{backend::CrosstermBackend, Terminal};
use ratatui_image::picker::Picker;
use tui::{InputAction, Tui};
use futures_util::stream::StreamExt;
use renderer::{RenderInfo, RenderNotif};

mod tui;
mod renderer;
mod converter;
mod skip;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
	let mut args = std::env::args().skip(1);
	let file = args.next().expect("Program requires a file to process");
	let path = PathBuf::from_str(&file)?.canonicalize()?;

	let (watch_tx, render_rx) = tokio::sync::mpsc::channel(1);
	let tui_tx = watch_tx.clone();

	// we need to call this outside the recommended_watcher call because if we call it inside, that
	// will be calling it from a thread not owned by the tokio runtime (since it's created by
	// calling thread::spawn) and that will cause a panic
	let mut watcher = notify::recommended_watcher(move |_| {
		// This shouldn't fail to send unless the receiver gets disconnected. If that's happened,
		// then like the main thread has panicked or something, so it doesn't matter if this panics
		// as well
		watch_tx.blocking_send(renderer::RenderNotif::Reload).unwrap();
	})?;

	// We're making this nonrecursive 'cause we're just watching a single file, so there's nothing
	// to recurse into
	watcher.watch(&path, RecursiveMode::NonRecursive)?;

	let file_path = format!("file://{}", path.clone().into_os_string().to_string_lossy());
	let (render_tx, mut tui_rx) = tokio::sync::mpsc::channel(1);

	// We need to create `picker` on this thread because if we create it on the `renderer` thread,
	// it messes up something with user input. Input never makes it to the crossterm thing
	let mut picker = Picker::from_termios()?;
	picker.guess_protocol();

	// then we want to spawn off the rendering task
	// We need to use the thread::spawn API so that this exists in a thread not owned by tokio,
	// since the methods we call in `start_rendering` will panic if called in an async context
	std::thread::spawn(move || { renderer::start_rendering(file_path, render_tx, render_rx) });

	let mut ev_stream = crossterm::event::EventStream::new();

	let file_name = path.file_name()
		.map(|n| n.to_string_lossy())
		.unwrap_or_else(|| "Unknown file".into())
		.to_string();
	let mut tui = tui::Tui::new(file_name);

	let backend = CrosstermBackend::new(std::io::stdout());
	let mut term = Terminal::new(backend)?;

	// poppler has some annoying logging (e.g. if you request a page index out-of-bounds of a
	// document's pages, then it will return `None`, but still log to stderr with CRITICAL level),
	// so we want to just ignore all logging since this is a tui app.
	glib::log_set_writer_func(noop);

	let mut converter = Converter::new(picker);

	execute!(
		term.backend_mut(),
		EnterAlternateScreen,
		crossterm::cursor::Hide
	)?;
	enable_raw_mode()?;

	let mut main_area = tui::Tui::main_layout(&term.get_frame());
	tui_tx.send(RenderNotif::Area(main_area[1])).await?;

	loop {
		let mut needs_redraw;

		tokio::select! {
			Some(img_res) = converter.next() => {
				match img_res {
					Ok((img, page)) => tui.page_ready(img, page),
					Err(e) => tui.show_error(e),
				}
				needs_redraw = true;
			},
			// First we check if we have any keystrokes
			Some(ev) = ev_stream.next() => {
				// If we can't get user input, just crash.
				let ev = ev.expect("Couldn't get any user input");

				needs_redraw = match tui.handle_event(ev) {
					None => false,
					Some(InputAction::Redraw) => true,
					Some(InputAction::QuitApp) => break,
					Some(InputAction::ChangePageBy(change)) => {
						converter.change_page_by(change);
						true
					},
					Some(InputAction::JumpingToPage(page)) => {
						tui_tx.send(RenderNotif::JumpToPage(page)).await?;
						converter.go_to_page(page);
						true
					}
				};
			},
			Some(renderer_msg) = tui_rx.recv() => {
				needs_redraw = match renderer_msg {
					Ok(RenderInfo::NumPages(num)) => {
						tui.set_n_pages(num);
						converter.set_n_pages(num);
						true
					},
					Ok(RenderInfo::Page(img, page_num)) => {
						converter.add_img(img, page_num);
						false
					},
					Err(e) => {
						tui.show_error(e);
						true
					}
				};
			}
		}

		let new_area = Tui::main_layout(&term.get_frame());
		if new_area != main_area {
			main_area = new_area;
			tui_tx.send(RenderNotif::Area(main_area[1])).await?;
			needs_redraw = true;
		}

		if needs_redraw {
			term.draw(|f| {
				tui.render(f, &main_area);
			})?;
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
