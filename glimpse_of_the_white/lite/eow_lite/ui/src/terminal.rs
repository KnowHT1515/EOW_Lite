use std::{fs::{File, OpenOptions}, future::pending, io, os::windows::io::AsRawHandle, time::Duration};

use crossterm::{cursor::Show, event::{DisableBracketedPaste, EnableBracketedPaste}, execute, style::{ResetColor, force_color_output}, terminal::{DisableLineWrap, EnableLineWrap, EnterAlternateScreen, LeaveAlternateScreen, size}};
use tokio::{io::{ReadHalf, WriteHalf}, net::windows::named_pipe::{ClientOptions, NamedPipeClient}, sync::mpsc, task::JoinHandle, time::{Instant, sleep_until}};
use unicode_segmentation::UnicodeSegmentation;
use windows_sys::Win32::{Foundation::HANDLE, System::Console::{ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_WINDOW_INPUT, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, GetConsoleMode, GetStdHandle, SetConsoleMode, SetStdHandle}};

use crate::ipc;
use crate::protocol::{ClientMessage, ContextEvent, ServerMessage};
use crate::render::View;
use crate::windows_input::{InputCommand, InputEvent, WindowsInput};

struct Console {
    original: [HANDLE; 3],
    input_mode: u32,
    _input: File,
    _output: File,
}

impl Console {

    fn open(system_console: bool) -> io::Result<Self> {
        let original: [HANDLE; 3] = unsafe { [GetStdHandle(STD_INPUT_HANDLE), GetStdHandle(STD_OUTPUT_HANDLE), GetStdHandle(STD_ERROR_HANDLE)] };
        let input: File = OpenOptions::new().read(true).write(true).open("CONIN$")?;
        let output: File = OpenOptions::new().read(true).write(true).open("CONOUT$")?;
        let mut input_mode: u32 = 0;
        if unsafe { GetConsoleMode(input.as_raw_handle(), &mut input_mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        unsafe {
            if SetStdHandle(STD_INPUT_HANDLE, input.as_raw_handle() as HANDLE) == 0 || SetStdHandle(STD_OUTPUT_HANDLE, output.as_raw_handle() as HANDLE) == 0 || SetStdHandle(STD_ERROR_HANDLE, output.as_raw_handle() as HANDLE) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let console: Self = Self { original, input_mode, _input: input, _output: output };
        force_color_output(true);
        let mut mode: u32 = (input_mode | ENABLE_EXTENDED_FLAGS | ENABLE_WINDOW_INPUT) & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT | ENABLE_QUICK_EDIT_MODE | ENABLE_VIRTUAL_TERMINAL_INPUT);
        if system_console {
            mode |= ENABLE_QUICK_EDIT_MODE;
        }
        if unsafe { SetConsoleMode(console._input.as_raw_handle(), mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        execute!(io::stdout(), EnterAlternateScreen, DisableLineWrap, EnableBracketedPaste)?;
        Ok(console)
    }

}

impl Drop for Console {

    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, ResetColor, DisableBracketedPaste, EnableLineWrap, LeaveAlternateScreen);
        unsafe {
            SetConsoleMode(self._input.as_raw_handle(), self.input_mode);
            SetStdHandle(STD_INPUT_HANDLE, self.original[0]);
            SetStdHandle(STD_OUTPUT_HANDLE, self.original[1]);
            SetStdHandle(STD_ERROR_HANDLE, self.original[2]);
        }
    }

}

struct Reader(JoinHandle<()>);

impl Drop for Reader {

    fn drop(&mut self) {
        self.0.abort();
    }

}

pub async fn run() -> io::Result<()> {
    let mut arguments: std::iter::Skip<std::env::ArgsOs> = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--pipe")) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "this frontend must be started by eow-lite-ui"));
    }
    let name: std::ffi::OsString = arguments.next().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing UI pipe"))?;
    let system_console: bool = match arguments.next().as_deref() {
        None => false,
        Some(argument) if argument == std::ffi::OsStr::new("--system-console") && arguments.next().is_none() => true,
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "unexpected frontend argument")),
    };
    let client: NamedPipeClient = ClientOptions::new().open(name)?;
    let _console: Console = Console::open(system_console)?;
    let (mut input, mut writer): (ReadHalf<NamedPipeClient>, WriteHalf<NamedPipeClient>) = tokio::io::split(client);
    let (sender, mut incoming): (mpsc::UnboundedSender<io::Result<ServerMessage>>, mpsc::UnboundedReceiver<io::Result<ServerMessage>>) = mpsc::unbounded_channel();
    let _reader: Reader = Reader(tokio::spawn(async move {
        loop {
            match ipc::receive(&mut input).await {
                Ok(Some(message)) => {
                    if sender.send(Ok(message)).is_err() {
                        break;
                    }
                },
                Ok(None) => break,
                Err(error) => {
                    let _ = sender.send(Err(error));
                    break;
                },
            }
        }
    }));
    ipc::send(&mut writer, &ClientMessage::Ready).await?;
    interact(&mut writer, &mut incoming).await
}

async fn interact(writer: &mut WriteHalf<NamedPipeClient>, incoming: &mut mpsc::UnboundedReceiver<io::Result<ServerMessage>>) -> io::Result<()> {
    let mut events: WindowsInput = WindowsInput::open()?;
    let mut output: io::Stdout = io::stdout();
    let mut view: View = View::new();
    let mut input: String = String::new();
    let mut cursor: usize = 0;
    let mut dimensions: (u16, u16) = size()?;
    let mut ready: bool = false;
    let mut waiting: bool = false;
    let mut active: Option<String> = None;
    let mut tick: Instant = Instant::now();
    view.draw(&mut output, &input, cursor, dimensions)?;
    loop {
        let was_typing: bool = view.typing();
        tokio::select! {
            message = incoming.recv() => {
                match message.transpose()? {
                    Some(ServerMessage::Ready) => ready = true,
                    Some(ServerMessage::Input { context_id, content }) => {
                        view.input(context_id.clone(), content);
                        active = Some(context_id);
                        waiting = true;
                    },
                    Some(ServerMessage::Context { event: ContextEvent::OutboundEnd { context_id } }) => {
                        view.event(ContextEvent::OutboundEnd { context_id: context_id.clone() })?;
                        if active.as_ref() == Some(&context_id) && !view.has_pending_text(&context_id) {
                            active = None;
                            waiting = false;
                        }
                    },
                    Some(ServerMessage::Context { event }) => view.event(event)?,
                    Some(ServerMessage::Rejected { context_id, reason }) => {
                        view.notice(format!("Request rejected: {reason}"));
                        if active.as_ref() == Some(&context_id) {
                            active = None;
                            waiting = false;
                        }
                    },
                    Some(ServerMessage::Notice { message }) => view.notice(message),
                    Some(ServerMessage::Close) | None => return Ok(()),
                }
            },
            event = events.recv() => {
                match event.transpose()? {
                    Some(InputEvent::Text(content)) => {
                        input.insert_str(cursor, &content);
                        cursor += content.len();
                    },
                    Some(InputEvent::Command { command, repeat }) => {
                        for _ in 0..repeat {
                            match command {
                                InputCommand::ScrollUp => view.scroll(-1, dimensions),
                                InputCommand::ScrollDown => view.scroll(1, dimensions),
                                InputCommand::PageUp => view.scroll(-(dimensions.1.saturating_sub(2) as isize), dimensions),
                                InputCommand::PageDown => view.scroll(dimensions.1.saturating_sub(2) as isize, dimensions),
                                InputCommand::Submit if ready && !waiting && !input.trim().is_empty() => {
                                    ipc::send(writer, &ClientMessage::Input { content: std::mem::take(&mut input) }).await?;
                                    cursor = 0;
                                    waiting = true;
                                },
                                _ => edit(&mut input, &mut cursor, command),
                            }
                        }
                    },
                    Some(InputEvent::Resize(width, height)) => dimensions = (width, height),
                    None => return Ok(()),
                }
            },
            _ = async {
                if was_typing {
                    sleep_until(tick).await;
                }
                else {
                    pending::<()>().await;
                }
            } => {
                view.advance();
                tick = next_tick();
            },
        }
        if !was_typing && view.typing() {
            tick = next_tick();
        }
        if active.as_ref().is_some_and(|context_id: &String| view.complete(context_id)) {
            active = None;
            waiting = false;
        }
        view.draw(&mut output, &input, cursor, dimensions)?;
    }
}

fn next_tick() -> Instant {
    Instant::now() + Duration::from_millis(rand::random_range(10..=20))
}

fn edit(input: &mut String, cursor: &mut usize, command: InputCommand) {
    match command {
        InputCommand::Left => *cursor = input[..*cursor].grapheme_indices(true).next_back().map_or(0, |(offset, _): (usize, &str)| offset),
        InputCommand::Right => *cursor += input[*cursor..].graphemes(true).next().map_or(0, str::len),
        InputCommand::Home => *cursor = 0,
        InputCommand::End => *cursor = input.len(),
        InputCommand::Backspace => {
            let start: usize = input[..*cursor].grapheme_indices(true).next_back().map_or(0, |(offset, _): (usize, &str)| offset);
            input.drain(start..*cursor);
            *cursor = start;
        },
        InputCommand::Delete => {
            let end: usize = *cursor + input[*cursor..].graphemes(true).next().map_or(0, str::len);
            input.drain(*cursor..end);
        },
        _ => {},
    }
}
