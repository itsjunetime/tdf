use std::{
	ffi::OsString,
	io::{stdout, Read, Write},
	num::NonZeroUsize,
	path::PathBuf
};

use crossterm::{
	execute,
	terminal::{
		disable_raw_mode, enable_raw_mode, window_size, EndSynchronizedUpdate,
		EnterAlternateScreen, LeaveAlternateScreen
	}
};
use futures_util::{stream::StreamExt, FutureExt};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use ratatui::{backend::CrosstermBackend, Terminal};
use ratatui_image::picker::Picker;
use tdf::{
	converter::{run_conversion_loop, ConvertedPage, ConverterMsg},
	renderer::{self, RenderError, RenderInfo, RenderNotif},
	tui::{BottomMessage, InputAction, MessageSetting, Tui}
};

// Dummy struct for easy errors in main
#[derive(Debug)]
struct BadTermSizeStdin(String);

impl std::fmt::Display for BadTermSizeStdin {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.0)
	}
}

impl std::error::Error for BadTermSizeStdin {}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
	#[cfg(feature = "tracing")]
	console_subscriber::init();

	let flags = xflags::parse_or_exit! {
		/// Display the pdf with the pages starting at the right hand size and moving left and
		/// adjust input keys to match
		optional -r,--r-to-l r_to_l: bool
		/// The maximum number of pages to display together, horizontally, at a time
		optional -m,--max-wide max_wide: NonZeroUsize
		/// PDF file to read
		required file: PathBuf
	};

	let path = flags.file.canonicalize()?;

	let (watch_to_render_tx, render_rx) = flume::unbounded();
	let tui_tx = watch_to_render_tx.clone();

	let (render_tx, tui_rx) = flume::unbounded();
	let watch_to_tui_tx = render_tx.clone();

	let mut watcher = notify::recommended_watcher(on_notify_ev(
		watch_to_tui_tx,
		watch_to_render_tx,
		path.file_name()
			.ok_or("Path does not have a last component??")?
			.to_owned()
	))?;

	// So we have to watch the parent directory of the file that we are interested in because the
	// `notify` library works on inodes, and if the file is deleted, that inode is gone as well,
	// and then the notify library just gives up on trying to watch for the file reappearing. Imo
	// they should start watching the parent directory if the file is deleted, and then wait for it
	// to reappear and then begin watching it again, but whatever. It seems they've made their
	// opinion on this clear
	// (https://github.com/notify-rs/notify/issues/113#issuecomment-281836995) so whatever, guess
	// we have to do this annoying workaround.
	watcher.watch(
		path.parent().expect("The root directory is not a PDF"),
		RecursiveMode::NonRecursive
	)?;

	// TODO: Handle non-utf8 file names? Maybe by constructing a CString and passing that in to the
	// mupdf stuff instead of a rust string?
	let file_path = path.clone().into_os_string().to_string_lossy().to_string();

	let mut window_size = window_size()?;

	if window_size.width == 0 || window_size.height == 0 {
		// send the command code to get the terminal window size
		print!("\x1b[14t");
		std::io::stdout().flush()?;

		// we need to enable raw mode here since this bit of output won't print a newline; it'll
		// just print the info it wants to tell us. So we want to get all characters as they come
		enable_raw_mode()?;

		// read in the returned size until we hit a 't' (which indicates to us it's done)
		let input_vec = std::io::stdin()
			.bytes()
			.filter_map(Result::ok)
			.take_while(|b| *b != b't')
			.collect::<Vec<_>>();

		// and then disable raw mode again in case we return an error in this next section
		disable_raw_mode()?;

		let input_line = String::from_utf8(input_vec)?;

		if input_line.starts_with("\x1b[4;") {
			// it should input it to us as `\e[4;<height>;<width>t`, so we need to split to get the h/w
			// ignore the first val
			let mut splits = input_line.split([';', 't']).skip(1);

			window_size.height = splits
				.next()
				.ok_or_else(|| {
					BadTermSizeStdin(format!(
						"Terminal responded with unparseable size response '{input_line}'"
					))
				})?
				.parse::<u16>()?;

			window_size.width = splits
				.next()
				.ok_or_else(|| {
					BadTermSizeStdin(format!(
						"Terminal responded with unparseable size response '{input_line}'"
					))
				})?
				.parse::<u16>()?;
		} else {
			return Err("Your terminal is falsely reporting a window size of 0; tdf needs an accurate window size to display graphics".into());
		}
	}

	// We need to create `picker` on this thread because if we create it on the `renderer` thread,
	// it messes up something with user input. Input never makes it to the crossterm thing
	let picker = Picker::from_query_stdio()?;

	// then we want to spawn off the rendering task
	// We need to use the thread::spawn API so that this exists in a thread not owned by tokio,
	// since the methods we call in `start_rendering` will panic if called in an async context
	std::thread::spawn(move || {
		renderer::start_rendering(&file_path, render_tx, render_rx, window_size)
	});

	let mut ev_stream = crossterm::event::EventStream::new();

	let (to_converter, from_main) = flume::unbounded();
	let (to_main, from_converter) = flume::unbounded();

	tokio::spawn(run_conversion_loop(to_main, from_main, picker, 20));

	let file_name = path.file_name().map_or_else(
		|| "Unknown file".into(),
		|n| n.to_string_lossy().to_string()
	);
	let mut tui = Tui::new(file_name, flags.max_wide, flags.r_to_l.unwrap_or_default());

	let backend = CrosstermBackend::new(std::io::stdout());
	let mut term = Terminal::new(backend)?;
	term.skip_diff(true);

	execute!(
		term.backend_mut(),
		EnterAlternateScreen,
		crossterm::cursor::Hide
	)?;
	enable_raw_mode()?;

	let mut main_area = Tui::main_layout(&term.get_frame());
	tui_tx.send(RenderNotif::Area(main_area[1]))?;

	let mut tui_rx = tui_rx.into_stream();
	let mut from_converter = from_converter.into_stream();

	loop {
		let mut needs_redraw = true;
		tokio::select! {
			// First we check if we have any keystrokes
			Some(ev) = ev_stream.next().fuse() => {
				// If we can't get user input, just crash.
				let ev = ev.expect("Couldn't get any user input");

				match tui.handle_event(&ev) {
					None => needs_redraw = false,
					Some(action) => match action {
						InputAction::Redraw => (),
						InputAction::QuitApp => break,
						InputAction::JumpingToPage(page) => {
							tui_tx.send(RenderNotif::JumpToPage(page))?;
							to_converter.send(ConverterMsg::GoToPage(page))?;
						},
						InputAction::Search(term) => tui_tx.send(RenderNotif::Search(term))?,
					}
				}
			},
			Some(renderer_msg) = tui_rx.next() => {
				match renderer_msg {
					Ok(render_info) => match render_info {
						RenderInfo::NumPages(num) => {
							tui.set_n_pages(num);
							to_converter.send(ConverterMsg::NumPages(num))?;
						},
						RenderInfo::Page(info) => {
							tui.got_num_results_on_page(info.page_num, info.result_rects.len());
							to_converter.send(ConverterMsg::AddImg(info))?;
						},
						RenderInfo::Reloaded => tui.set_msg(MessageSetting::Some(BottomMessage::Reloaded)),
					},
					Err(e) => tui.show_error(e),
				}
			}
			Some(img_res) = from_converter.next() => {
				match img_res {
					Ok(ConvertedPage { page, num, num_results }) => tui.page_ready(page, num, num_results),
					Err(e) => tui.show_error(e),
				}
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

fn on_notify_ev(
	to_tui_tx: flume::Sender<Result<RenderInfo, RenderError>>,
	to_render_tx: flume::Sender<RenderNotif>,
	file_name: OsString
) -> impl Fn(notify::Result<Event>) {
	move |res| match res {
		// If we get an error here, and then an error sending, everything's going wrong. Just give
		// up lol.
		Err(e) => to_tui_tx.send(Err(RenderError::Notify(e))).unwrap(),
		// TODO: Should we match EventKind::Rename and propogate that so that the other parts of the
		// process know that too? Or should that be
		Ok(ev) => {
			// We only watch the parent directory (see the comment above `watcher.watch` in `fn
			// main`) so we need to filter out events to only ones that pertain to the single file
			// we care about
			if !ev
				.paths
				.iter()
				.any(|path| path.file_name().is_some_and(|f| f == file_name))
			{
				return;
			}

			match ev.kind {
				EventKind::Access(_) => (),
				EventKind::Remove(_) => to_tui_tx
					.send(Err(RenderError::Converting("File was deleted".into())))
					.unwrap(),
				// This shouldn't fail to send unless the receiver gets disconnected. If that's
				// happened, then like the main thread has panicked or something, so it doesn't matter
				// we don't handle the error here.
				EventKind::Other | EventKind::Any | EventKind::Create(_) | EventKind::Modify(_) =>
					to_render_tx.send(RenderNotif::Reload).unwrap(),
			}
		}
	}
}
