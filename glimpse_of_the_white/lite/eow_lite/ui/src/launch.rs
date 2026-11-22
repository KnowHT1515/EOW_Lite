use std::{ffi::OsStr, io, os::windows::ffi::OsStrExt, os::windows::process::CommandExt, path::{Path, PathBuf}, process::{Child, Command, Stdio}, time::Duration};

use tokio::{io::{AsyncWriteExt, ReadHalf, WriteHalf}, net::windows::named_pipe::{NamedPipeServer, ServerOptions}, sync::mpsc, task::JoinHandle, time::{sleep, timeout}};
use windows_sys::Win32::{System::{Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize}, Threading::CREATE_NEW_CONSOLE}, UI::{Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW}, WindowsAndMessaging::SW_SHOWNORMAL}};

use crate::ipc;
use crate::protocol::{ClientMessage, ServerMessage};

pub struct Session {
    writer: WriteHalf<NamedPipeServer>,
    pub incoming: mpsc::UnboundedReceiver<io::Result<ClientMessage>>,
    reader: JoinHandle<()>,
    child: Option<Child>,
}

impl Session {

    pub async fn open() -> io::Result<Self> {
        let executable: PathBuf = std::env::current_exe()?.with_file_name("eow-lite-ui-terminal.exe");
        if !executable.is_file() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} was not found", executable.display())));
        }
        match Self::attempt(&executable, true).await {
            Ok(session) => Ok(session),
            Err(error) => {
                eprintln!("ui: Windows Terminal unavailable; creating a system console: {error}");
                Self::attempt(&executable, false).await
            },
        }
    }

    async fn attempt(executable: &Path, terminal: bool) -> io::Result<Self> {
        let pipe: String = format!(r"\\.\pipe\eow-lite-ui-{}-{:016x}", std::process::id(), rand::random::<u64>());
        let mut server: NamedPipeServer = ServerOptions::new().first_pipe_instance(true).max_instances(1).reject_remote_clients(true).create(&pipe)?;
        let mut child: Option<Child> = if terminal {
            launch_terminal(executable, &pipe)?;
            None
        }
        else {
            Some(Command::new(executable).arg("--pipe").arg(&pipe).arg("--system-console").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).creation_flags(CREATE_NEW_CONSOLE).spawn()?)
        };
        let ready: io::Result<()> = match timeout(Duration::from_secs(10), async {
            server.connect().await?;
            match ipc::receive::<ClientMessage>(&mut server).await? {
                Some(ClientMessage::Ready) => Ok(()),
                _ => Err(io::Error::new(io::ErrorKind::InvalidData, "terminal frontend did not become ready")),
            }
        }).await {
            Ok(result) => result,
            Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "terminal frontend startup timed out")),
        };
        if let Err(error) = ready {
            if let Some(child) = &mut child {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err(error);
        }
        let (mut input, writer): (ReadHalf<NamedPipeServer>, WriteHalf<NamedPipeServer>) = tokio::io::split(server);
        let (sender, incoming): (mpsc::UnboundedSender<io::Result<ClientMessage>>, mpsc::UnboundedReceiver<io::Result<ClientMessage>>) = mpsc::unbounded_channel();
        let reader: JoinHandle<()> = tokio::spawn(async move {
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
        });
        Ok(Self { writer, incoming, reader, child })
    }

    pub async fn send(&mut self, message: &ServerMessage) -> io::Result<()> {
        ipc::send(&mut self.writer, message).await
    }

    pub async fn close(&mut self) {
        let _ = timeout(Duration::from_secs(1), self.send(&ServerMessage::Close)).await;
        let _ = self.writer.shutdown().await;
        let _ = timeout(Duration::from_secs(1), &mut self.reader).await;
        if let Some(child) = &mut self.child {
            let exited: Result<io::Result<()>, tokio::time::error::Elapsed> = timeout(Duration::from_secs(1), async {
                loop {
                    if child.try_wait()?.is_some() {
                        return Ok(());
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            }).await;
            if !matches!(exited, Ok(Ok(()))) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }

}

impl Drop for Session {

    fn drop(&mut self) {
        self.reader.abort();
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

}

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

fn quote(value: &OsStr) -> String {
    let mut quoted: String = String::from("\"");
    let mut slashes: usize = 0;
    for character in value.to_string_lossy().chars() {
        if character == '\\' {
            slashes += 1;
        }
        else {
            quoted.push_str(&"\\".repeat(if character == '"' { slashes * 2 + 1 } else { slashes }));
            slashes = 0;
            if character == ';' {
                quoted.push('\\');
            }
            quoted.push(character);
        }
    }
    quoted.push_str(&"\\".repeat(slashes * 2));
    quoted.push('"');
    quoted
}

fn launch_terminal(executable: &Path, pipe: &str) -> io::Result<()> {
    let file: Vec<u16> = wide("wt.exe");
    let parameters: Vec<u16> = wide(format!("-w new new-tab --title \"EOW Lite UI\" {} --pipe {}", quote(executable.as_os_str()), quote(OsStr::new(pipe))));
    let directory: Vec<u16> = wide(executable.parent().ok_or_else(|| io::Error::other("missing UI executable directory"))?);
    let mut info: SHELLEXECUTEINFOW = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        lpDirectory: directory.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..SHELLEXECUTEINFOW::default()
    };
    let initialized: i32 = unsafe { CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32) };
    if initialized < 0 {
        return Err(io::Error::other(format!("initialize terminal launcher: HRESULT {initialized:#010x}")));
    }
    let launched: bool = unsafe { ShellExecuteExW(&mut info) != 0 };
    let result: io::Result<()> = if launched { Ok(()) } else { Err(io::Error::last_os_error()) };
    unsafe { CoUninitialize(); }
    result
}
