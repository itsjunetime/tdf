use core::{
	error::Error,
	num::{NonZeroU32, NonZeroUsize}
};
use std::{
	ffi::OsString,
	io::{BufReader, Read, Stdout, Write, stdout},
	path::PathBuf
};

use crossterm::{
	event::EventStream,
	execute,
	terminal::{
		EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
		enable_raw_mode, window_size
	}
};
use flexi_logger::FileSpec;
use flume::{Sender, r#async::RecvStream};
use futures_util::{FutureExt, stream::StreamExt};
use kittage::{
	action::Action,
	delete::{ClearOrDelete, DeleteConfig, WhichToDelete},
	error::{TerminalError, TransmitError}
};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use ratatui::{Terminal, backend::CrosstermBackend};
use ratatui_image::{
	FontSize,
	picker::{Picker, ProtocolType}
};
use tdf::{
	PrerenderLimit, WrappedErr,
	converter::{ConvertedPage, ConverterMsg, run_conversion_loop},
	history::DocumentHistory,
	kitty::{KittyDisplay, display_kitty_images, do_shms_work, run_action},
	renderer::{self, RenderError, RenderInfo, RenderNotif},
	tui::{BottomMessage, InputAction, MessageSetting, Tui}
};

fn reset_term() {
	_ = execute!(
		std::io::stdout(),
		LeaveAlternateScreen,
		crossterm::cursor::Show,
		crossterm::event::DisableMouseCapture
	)
}

#[tokio::main]
async fn main() -> Result<(), WrappedErr> {
	inner_main().await.inspect_err(|_| reset_term())
}

async fn inner_main() -> Result<(), WrappedErr> {
	let hook = std::panic::take_hook();
	std::panic::set_hook(Box::new(move |info| {
		reset_term();
		hook(info);
	}));

	#[cfg(feature = "tracing")]
	console_subscriber::init();

	let flags = xflags::parse_or_exit! {
		/// Display the pdf with the pages starting at the right hand size and moving left and
		/// adjust input keys to match
		optional -r,--r-to-l r_to_l: bool
		/// The maximum number of pages to display together, horizontally, at a time
		optional -m,--max-wide max_wide: NonZeroUsize
		/// Fullscreen the pdf (hide document name, page count, etc)
		optional -f,--fullscreen fullscreen: bool
		/// The number of pages to prerender surrounding the currently-shown page; 0 means no
		/// limit. By default, there is no limit.
		optional -p,--prerender prerender: usize
		/// Custom white color, specified in css format (e.g. "FFFFFF" or "rgb(255, 255, 255)")
		optional -w,--white-color white: String
		/// Custom black color, specified in css format (e.g "000000" or "rgb(0, 0, 0)")
		optional -b,--black-color black: String
		/// Print the version and exit
		optional --version
		/// PDF file to read
		optional file: PathBuf
	};

	if flags.version {
		println!("{}", env!("CARGO_PKG_VERSION"));
		return Ok(());
	}

	let Some(file) = flags.file else {
		return Err(WrappedErr(
			"Please specify the file to open, e.g. `tdf ./my_example_pdf.pdf`".into()
		));
	};

	let path = file
		.canonicalize()
		.map_err(|e| WrappedErr(format!("Cannot canonicalize provided file: {e}").into()))?;

	let black =
		parse_color_to_i32(flags.black_color.as_deref().unwrap_or("000000")).map_err(|e| {
			WrappedErr(
				format!("Couldn't parse black color: {e} - is it formatted like a CSS color?")
					.into()
			)
		})?;

	let white =
		parse_color_to_i32(flags.white_color.as_deref().unwrap_or("FFFFFF")).map_err(|e| {
			WrappedErr(
				format!("Couldn't parse white color: {e} - is it formatted like a CSS color?")
					.into()
			)
		})?;

	// need to keep it around throughout the lifetime of the program, but don't rly need to use it.
	// Just need to make sure it doesn't get dropped yet.
	let mut maybe_logger = None;

	if std::env::var("RUST_LOG").is_ok() {
		maybe_logger = Some(
			flexi_logger::Logger::try_with_env()
				.map_err(|e| WrappedErr(format!("Couldn't create initial logger: {e}").into()))?
				.log_to_file(FileSpec::try_from("./debug.log").map_err(|e| {
					WrappedErr(format!("Couldn't create FileSpec for logger: {e}").into())
				})?)
				.start()
				.map_err(|e| WrappedErr(format!("Can't start logger: {e}").into()))?
		);
	}

	let (watch_to_render_tx, render_rx) = flume::unbounded();
	let to_renderer = watch_to_render_tx.clone();

	let (render_tx, tui_rx) = flume::unbounded();
	let watch_to_tui_tx = render_tx.clone();

	let mut watcher = notify::recommended_watcher(on_notify_ev(
		watch_to_tui_tx,
		watch_to_render_tx,
		path.file_name()
			.ok_or(WrappedErr("Path does not have a last component??".into()))?
			.to_owned()
	))
	.map_err(|e| WrappedErr(format!("Couldn't start watching the provided file: {e}").into()))?;

	// So we have to watch the parent directory of the file that we are interested in because the
	// `notify` library works on inodes, and if the file is deleted, that inode is gone as well,
	// and then the notify library just gives up on trying to watch for the file reappearing. Imo
	// they should start watching the parent directory if the file is deleted, and then wait for it
	// to reappear and then begin watching it again, but whatever. It seems they've made their
	// opinion on this clear
	// (https://github.com/notify-rs/notify/issues/113#issuecomment-281836995) so whatever, guess
	// we have to do this annoying workaround.
	watcher
		.watch(
			path.parent().expect("The root directory is not a PDF"),
			RecursiveMode::NonRecursive
		)
		.map_err(|e| WrappedErr(format!("Can't watch the provided file: {e}").into()))?;

	// TODO: Handle non-utf8 file names? Maybe by constructing a CString and passing that in to the
	// mupdf stuff instead of a rust string?
	let file_path = path.clone().into_os_string().to_string_lossy().to_string();

	let mut window_size = window_size().map_err(|e| {
		WrappedErr(format!("Can't get your current terminal window size: {e}").into())
	})?;

	if window_size.width == 0 || window_size.height == 0 {
		let (w, h) = get_font_size_through_stdio()?;

		window_size.width = w;
		window_size.height = h;
	}

	let cell_height_px = window_size.height / window_size.rows;
	let cell_width_px = window_size.width / window_size.columns;

	execute!(
		std::io::stdout(),
		EnterAlternateScreen,
		crossterm::cursor::Hide,
		crossterm::event::EnableMouseCapture
	)
	.map_err(|e| {
		WrappedErr(
			format!(
				"Couldn't enter the alternate screen and hide the cursor for proper presentation: {e}"
			)
			.into()
		)
	})?;

	// We need to create `picker` on this thread because if we create it on the `renderer` thread,
	// it messes up something with user input. Input never makes it to the crossterm thing
	let picker = Picker::from_query_stdio()
		.or_else(|e| match e {
			ratatui_image::errors::Errors::NoFontSize if
				window_size.width != 0
				&& window_size.height != 0
				&& window_size.columns != 0
				&& window_size.rows != 0
					=> Ok(Picker::from_fontsize((cell_width_px, cell_height_px))),
			ratatui_image::errors::Errors::NoFontSize => Err(WrappedErr(
				"Unable to detect your terminal's font size; this is an issue with your terminal emulator.\nPlease use a different terminal emulator or report this bug to tdf.".into()
			)),
			e => Err(WrappedErr(format!("Couldn't get the necessary information to set up images: {e}").into()))
		})?;

	// then we want to spawn off the rendering task
	// We need to use the thread::spawn API so that this exists in a thread not owned by tokio,
	// since the methods we call in `start_rendering` will panic if called in an async context
	let prerender = flags
		.prerender
		.and_then(NonZeroUsize::new)
		.map_or(PrerenderLimit::All, PrerenderLimit::Limited);

	std::thread::spawn(move || {
		renderer::start_rendering(
			&file_path,
			render_tx,
			render_rx,
			cell_height_px,
			cell_width_px,
			prerender,
			black,
			white
		)
	});

	let font_size = picker.font_size();

	let mut ev_stream = crossterm::event::EventStream::new();

	let (to_converter, from_main) = flume::unbounded();
	let (to_main, from_converter) = flume::unbounded();

	let is_kitty = picker.protocol_type() == ProtocolType::Kitty;

	let shms_work = is_kitty && do_shms_work(&mut ev_stream).await;

	tokio::spawn(run_conversion_loop(
		to_main, from_main, picker, 20, shms_work
	));

	let file_name = path.file_name().map_or_else(
		|| "Unknown file".into(),
		|n| n.to_string_lossy().to_string()
	);
	let mut tui = Tui::new(
		file_name,
		flags.max_wide,
		flags.r_to_l.unwrap_or_default(),
		is_kitty
	);
	let mut document_history = DocumentHistory::load().unwrap_or_else(|e| {
		WrappedErr(format!("Couldn't initialize document history: {e}").into());
		DocumentHistory::default()
	});
	let restored_page = document_history
		.last_pages_opened
		.get(&path.to_string_lossy().to_string())
		.copied();

	let backend = CrosstermBackend::new(std::io::stdout());
	let mut term = Terminal::new(backend).map_err(|e| {
		WrappedErr(format!("Couldn't set up crossterm's terminal backend: {e}").into())
	})?;
	term.skip_diff(true);

	enable_raw_mode().map_err(|e| {
		WrappedErr(
			format!("Can't enable raw mode, which is necessary to receive input: {e}").into()
		)
	})?;

	if is_kitty {
		run_action(
			Action::Delete(DeleteConfig {
				effect: ClearOrDelete::Delete,
				which: WhichToDelete::IdRange(NonZeroU32::new(1).unwrap()..=NonZeroU32::MAX)
			}),
			&mut ev_stream
		)
		.await
		.map_err(|e| {
			WrappedErr(format!("Couldn't delete all previous images from memory: {e}").into())
		})?;
	}

	let fullscreen = flags.fullscreen.unwrap_or_default();
	let main_area = Tui::main_layout(&term.get_frame(), fullscreen);
	to_renderer
		.send(RenderNotif::Area(main_area.page_area))
		.map_err(|e| {
			WrappedErr(
				format!("Couldn't inform the rendering thread of the available area: {e}").into()
			)
		})?;

	let tui_rx = tui_rx.into_stream();
	let from_converter = from_converter.into_stream();

	if let Some(page) = restored_page {
		tui.set_page(page);
		to_renderer
			.send(RenderNotif::JumpToPage(page))
			.map_err(|e| {
				WrappedErr(format!("Couldn't tell renderer to jump to restored page: {e}").into())
			})?;
		to_converter
			.send(ConverterMsg::GoToPage(page))
			.map_err(|e| {
				WrappedErr(format!("Couldn't tell converter to jump to restored page: {e}").into())
			})?;
	}

	enter_redraw_loop(
		ev_stream,
		to_renderer,
		tui_rx,
		to_converter,
		from_converter,
		fullscreen,
		&mut tui,
		&mut term,
		main_area,
		font_size
	)
	.await
	.map_err(|e| {
		WrappedErr(
			format!(
				"An unexpected error occurred while communicating between different parts of tdf: {e}"
			)
			.into()
		)
	})?;

	execute!(
		term.backend_mut(),
		LeaveAlternateScreen,
		crossterm::cursor::Show,
		crossterm::event::DisableMouseCapture
	)
	.unwrap();
	disable_raw_mode().unwrap();

	drop(maybe_logger);

	document_history
		.last_pages_opened
		.insert(path.to_string_lossy().to_string(), tui.page);

	if let Err(e) = document_history.save() {
		WrappedErr(format!("Failed to save last opened page: {e}").into());
	}

	Ok(())
}

// oh shut up clippy who cares
#[expect(clippy::too_many_arguments)]
async fn enter_redraw_loop(
	mut ev_stream: EventStream,
	to_renderer: Sender<RenderNotif>,
	mut tui_rx: RecvStream<'_, Result<RenderInfo, RenderError>>,
	to_converter: Sender<ConverterMsg>,
	mut from_converter: RecvStream<'_, Result<ConvertedPage, RenderError>>,
	mut fullscreen: bool,
	tui: &mut Tui,
	term: &mut Terminal<CrosstermBackend<Stdout>>,
	mut main_area: tdf::tui::RenderLayout,
	font_size: FontSize
) -> Result<(), Box<dyn Error>> {
	loop {
		let mut needs_redraw = true;
		let next_ev = ev_stream.next().fuse();
		tokio::select! {
			// First we check if we have any keystrokes
			Some(ev) = next_ev => {
				// If we can't get user input, just crash.
				let ev = ev.expect("Couldn't get any user input");

				match tui.handle_event(&ev) {
					None => needs_redraw = false,
					Some(action) => match action {
						InputAction::Redraw => (),
						InputAction::QuitApp => return Ok(()),
						InputAction::JumpingToPage(page) => {
							to_renderer.send(RenderNotif::JumpToPage(page))?;
							to_converter.send(ConverterMsg::GoToPage(page))?;
						},
						InputAction::Search(term) => to_renderer.send(RenderNotif::Search(term))?,
						InputAction::Invert => to_renderer.send(RenderNotif::Invert)?,
						InputAction::Fullscreen => fullscreen = !fullscreen,
						InputAction::SwitchRenderZoom(f_or_f) => {
							to_renderer.send(RenderNotif::SwitchFitOrFill(f_or_f)).unwrap();
						}
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
						RenderInfo::SearchResults { page_num, num_results } =>
							tui.got_num_results_on_page(page_num, num_results),
					},
					Err(e) => tui.show_error(e),
				}
			}
			Some(img_res) = from_converter.next() => {
				match img_res {
					Ok(ConvertedPage { page, num, num_results }) => {
						tui.page_ready(page, num, num_results);
						if num == tui.page {
							needs_redraw = true;
						}
					},
					Err(e) => tui.show_error(e),
				}
			},
		};

		let new_area = Tui::main_layout(&term.get_frame(), fullscreen);
		if new_area != main_area {
			main_area = new_area;
			to_renderer.send(RenderNotif::Area(main_area.page_area))?;
			needs_redraw = true;
		}

		if needs_redraw {
			let mut to_display = KittyDisplay::NoChange;
			term.draw(|f| {
				to_display = tui.render(f, &main_area, font_size);
			})?;

			let maybe_err = display_kitty_images(to_display, &mut ev_stream).await;

			if let Err((to_replace, err_desc, enum_err)) = maybe_err {
				match enum_err {
					// This is the error that kitty & ghostty provide us when they delete an
					// image due to memory constraints, so if we get it, we just fix it by
					// re-rendering so it don't display it to the user
					//
					// [TODO] maybe when we detect that an image was deleted, we probe the
					// terminal for the pages around it to see if they were deleted too and if
					// they were, we re-render them? idk
					TransmitError::Terminal(TerminalError::NoEntity(_)) => (),
					_ => tui.set_msg(MessageSetting::Some(BottomMessage::Error(format!(
						"{err_desc}: {enum_err}"
					))))
				}

				for page_num in to_replace {
					tui.page_failed_display(page_num);
					// So that they get re-rendered and sent over again
					to_renderer.send(RenderNotif::PageNeedsReRender(page_num))?;
				}
			}

			execute!(stdout().lock(), EndSynchronizedUpdate)?;
		}
	}
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

fn parse_color_to_i32(cs: &str) -> Result<i32, csscolorparser::ParseColorError> {
	let color = csscolorparser::parse(cs)?;
	let [r, g, b, _] = color.to_rgba8();
	Ok(i32::from_be_bytes([0, r, g, b]))
}

fn get_font_size_through_stdio() -> Result<(u16, u16), WrappedErr> {
	// send the command code to get the terminal window size
	print!("\x1b[14t");
	std::io::stdout().flush().unwrap();

	// we need to enable raw mode here since this bit of output won't print a newline; it'll
	// just print the info it wants to tell us. So we want to get all characters as they come
	enable_raw_mode().map_err(|e| {
		WrappedErr(
			format!("Can't enable raw mode, which is necessary to receive input: {e}").into()
		)
	})?;

	// read in the returned size until we hit a 't' (which indicates to us it's done)
	let input_vec = BufReader::new(std::io::stdin())
		.bytes()
		.filter_map(Result::ok)
		.take_while(|b| *b != b't')
		.collect::<Vec<_>>();

	// and then disable raw mode again in case we return an error in this next section
	disable_raw_mode().map_err(|e| {
		WrappedErr(format!("Can't put the terminal back into a normal input state: {e}").into())
	})?;

	let input_line = String::from_utf8(input_vec).map_err(|e| {
		WrappedErr(
			format!(
				"The terminal responded to our request for its font size by providing non-utf8 data: {e}"
			)
			.into()
		)
	})?;
	let input_line = input_line
		.trim_start_matches("\x1b[4")
		.trim_start_matches(';');

	// it should input it to us as `\e[4;<height>;<width>t`, so we need to split to get the h/w
	// ignore the first val
	let mut splits = input_line.split([';', 't']);

	let (Some(h), Some(w)) = (splits.next(), splits.next()) else {
		return Err(WrappedErr(
			format!("Terminal responded with unparseable size response '{input_line}'").into()
		));
	};

	let h = h.parse::<u16>().map_err(|_| {
		WrappedErr(
			format!(
				"Your terminal said its height is {h}, but that is not a 16-bit unsigned integer"
			)
			.into()
		)
	})?;
	let w = w.parse::<u16>().map_err(|_| {
		WrappedErr(
			format!(
				"Your terminal said its width is {w}, but that is not a 16-bit unsigned integer"
			)
			.into()
		)
	})?;

	Ok((w, h))
}
