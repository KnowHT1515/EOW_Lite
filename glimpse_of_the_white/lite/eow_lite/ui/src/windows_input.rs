use std::{fs::{File, OpenOptions}, io, os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle}, sync::Arc, thread::{self, JoinHandle}};

use crossterm::terminal::size;
use tokio::sync::mpsc;
use windows_sys::Win32::{Foundation::WAIT_OBJECT_0, System::{Console::{INPUT_RECORD, KEY_EVENT, KEY_EVENT_RECORD, LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED, RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED, ReadConsoleInputW, WINDOW_BUFFER_SIZE_EVENT}, Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects}}, UI::Input::KeyboardAndMouse::{VK_BACK, VK_C, VK_DELETE, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_UP}};

pub enum InputEvent {
    Text(String),
    Command { command: InputCommand, repeat: u16 },
    Resize(u16, u16),
}

#[derive(Clone, Copy)]
pub enum InputCommand {
    Left,
    Right,
    Home,
    End,
    Backspace,
    Delete,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    Submit,
}

pub struct WindowsInput {
    incoming: mpsc::UnboundedReceiver<io::Result<InputEvent>>,
    stop: Arc<OwnedHandle>,
    worker: Option<JoinHandle<()>>,
}

impl WindowsInput {

    pub fn open() -> io::Result<Self> {
        let input: File = OpenOptions::new().read(true).open("CONIN$")?;
        let handle: windows_sys::Win32::Foundation::HANDLE = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let stop: Arc<OwnedHandle> = Arc::new(unsafe { OwnedHandle::from_raw_handle(handle) });
        let stopping: Arc<OwnedHandle> = Arc::clone(&stop);
        let (sender, incoming): (mpsc::UnboundedSender<io::Result<InputEvent>>, mpsc::UnboundedReceiver<io::Result<InputEvent>>) = mpsc::unbounded_channel();
        let worker: JoinHandle<()> = thread::Builder::new().name("ui-console-input".into()).spawn(move || {
            if let Err(error) = read_events(&input, &stopping, &sender) {
                let _ = sender.send(Err(error));
            }
        })?;
        Ok(Self { incoming, stop, worker: Some(worker) })
    }

    pub async fn recv(&mut self) -> Option<io::Result<InputEvent>> {
        self.incoming.recv().await
    }

}

impl Drop for WindowsInput {

    fn drop(&mut self) {
        unsafe { SetEvent(self.stop.as_raw_handle()); }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

}

fn read_events(input: &File, stop: &OwnedHandle, sender: &mpsc::UnboundedSender<io::Result<InputEvent>>) -> io::Result<()> {
    let handles: [windows_sys::Win32::Foundation::HANDLE; 2] = [stop.as_raw_handle(), input.as_raw_handle()];
    let mut decoder: InputDecoder = InputDecoder::default();
    loop {
        let ready: u32 = unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE) };
        if ready == WAIT_OBJECT_0 {
            return Ok(());
        }
        if ready != WAIT_OBJECT_0 + 1 {
            return Err(io::Error::last_os_error());
        }
        let mut records: [INPUT_RECORD; 64] = [INPUT_RECORD::default(); 64];
        let mut count: u32 = 0;
        if unsafe { ReadConsoleInputW(input.as_raw_handle(), records.as_mut_ptr(), records.len() as u32, &mut count) } == 0 {
            return Err(io::Error::last_os_error());
        }
        for record in &records[..count as usize] {
            let event: Option<InputEvent> = match u32::from(record.EventType) {
                KEY_EVENT => decoder.key(unsafe { record.Event.KeyEvent }),
                WINDOW_BUFFER_SIZE_EVENT => {
                    let (width, height): (u16, u16) = size()?;
                    Some(InputEvent::Resize(width, height))
                },
                _ => None,
            };
            if let Some(event) = event
                && sender.send(Ok(event)).is_err() {
                return Ok(());
            }
        }
    }
}

#[derive(Default)]
struct InputDecoder {
    high_surrogate: Option<u16>,
}

impl InputDecoder {

    fn key(&mut self, key: KEY_EVENT_RECORD) -> Option<InputEvent> {
        let unit: u16 = unsafe { key.uChar.UnicodeChar };
        if key.bKeyDown == 0 {
            return if key.wVirtualKeyCode == VK_MENU && unit != 0 { self.text(unit, key.wRepeatCount) } else { None };
        }
        let control: bool = key.dwControlKeyState & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0;
        let alt: bool = key.dwControlKeyState & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0;
        if unit == 3 || (key.wVirtualKeyCode == VK_C && control && !alt) {
            self.high_surrogate = None;
            return None;
        }
        if control || alt {
            return if control && alt && unit != 0 { self.text(unit, key.wRepeatCount) } else { None };
        }
        let command: Option<InputCommand> = match key.wVirtualKeyCode {
            VK_BACK => Some(InputCommand::Backspace),
            VK_DELETE => Some(InputCommand::Delete),
            VK_LEFT => Some(InputCommand::Left),
            VK_RIGHT => Some(InputCommand::Right),
            VK_HOME => Some(InputCommand::Home),
            VK_END => Some(InputCommand::End),
            VK_UP => Some(InputCommand::ScrollUp),
            VK_DOWN => Some(InputCommand::ScrollDown),
            VK_PRIOR => Some(InputCommand::PageUp),
            VK_NEXT => Some(InputCommand::PageDown),
            VK_RETURN if unit == 13 => Some(InputCommand::Submit),
            _ => None,
        };
        if let Some(command) = command {
            self.high_surrogate = None;
            let repeat: u16 = if matches!(command, InputCommand::Submit) { 1 } else { key.wRepeatCount.max(1) };
            return Some(InputEvent::Command { command, repeat });
        }
        self.text(unit, key.wRepeatCount)
    }

    fn text(&mut self, unit: u16, repeat: u16) -> Option<InputEvent> {
        let character: char = match unit {
            0 => return None,
            0xd800..=0xdbff => {
                self.high_surrogate = Some(unit);
                return None;
            },
            0xdc00..=0xdfff => {
                let high: u16 = self.high_surrogate.take()?;
                char::from_u32(0x10000 + ((u32::from(high) - 0xd800) << 10) + u32::from(unit) - 0xdc00)?
            },
            _ => {
                self.high_surrogate = None;
                char::from_u32(u32::from(unit))?
            },
        };
        if character.is_control() {
            return None;
        }
        Some(InputEvent::Text(character.to_string().repeat(usize::from(repeat.max(1)))))
    }

}
