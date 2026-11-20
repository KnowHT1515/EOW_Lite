use std::{io::{self, BufRead, BufReader, Write}, path::Path, process::{Child, Command, ExitStatus, Stdio}, sync::mpsc, thread, time::{Duration, Instant}};

use eow_lite_helper::lifecycle::{BOOTSTRAP, SHUTDOWN};

use crate::plan::ModuleSpec;
use crate::platform::ProcessContainer;

pub const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug)]
pub enum StopOutcome {
    Graceful,
    Forced,
    AlreadyExited,
}

pub struct ManagedProcess<'a> {
    pub spec: &'a ModuleSpec,
    pub name: &'a str,
    child: Child,
    first_line: mpsc::Receiver<io::Result<String>>,
    stdout_reader: Option<thread::JoinHandle<()>>,
}

impl<'a> ManagedProcess<'a> {

    pub fn spawn(spec: &'a ModuleSpec, name: &'a str, executable: &Path, directory: &Path, container: &ProcessContainer) -> io::Result<Self> {
        let mut command: Command = Command::new(executable);
        command
            .arg("supervised")
            .arg(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        container.prepare(&mut command);
        let mut child: Child = command.spawn()?;
        if let Err(err) = container.assign(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }

        let stdout: std::process::ChildStdout = child.stdout.take().expect("piped module stdout");
        let (sender, first_line): (mpsc::Sender<io::Result<String>>, mpsc::Receiver<io::Result<String>>) = mpsc::channel();
        let offline_marker: String = format!("{name} Offline");
        let stdout_reader: std::thread::JoinHandle<()> = thread::spawn(move || {
            let mut lines: std::io::Lines<BufReader<std::process::ChildStdout>> = BufReader::new(stdout).lines();
            if let Some(line) = lines.next() {
                let _ = sender.send(line);
                for line in lines.map_while(Result::ok) {
                    if line != offline_marker {
                        println!("{line}");
                    }
                }
            }
        });
        Ok(Self {
            spec,
            name,
            child,
            first_line,
            stdout_reader: Some(stdout_reader),
        })
    }

    pub fn bootstrap(&mut self) -> io::Result<()> {
        let stdin: &mut std::process::ChildStdin = self.child.stdin.as_mut().expect("piped module stdin");
        writeln!(stdin, "{BOOTSTRAP}")?;
        stdin.flush()
    }

    pub fn online(&self) -> io::Result<bool> {
        match self.first_line.try_recv() {
            Ok(Ok(line)) if line == format!("{} Online", self.name) => Ok(true),
            Ok(Ok(line)) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected readiness line: {line:?}"),
            )),
            Ok(Err(err)) => Err(err),
            Err(mpsc::TryRecvError::Empty) => Ok(false),
            Err(mpsc::TryRecvError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stdout closed before Online",
            )),
        }
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn stop(&mut self) -> io::Result<StopOutcome> {
        let result: io::Result<StopOutcome> = self.stop_child();
        if (result.is_ok() || self.child.try_wait().is_ok_and(|status: Option<ExitStatus>| status.is_some()))
            && let Some(reader) = self.stdout_reader.take()
        {
            let _ = reader.join();
        }
        result
    }

    fn stop_child(&mut self) -> io::Result<StopOutcome> {
        if self.child.try_wait()?.is_some() {
            return Ok(StopOutcome::AlreadyExited);
        }
        if let Some(mut stdin) = self.child.stdin.take() {
            let _ = writeln!(stdin, "{SHUTDOWN}");
        }
        let timeout: Duration = Duration::from_millis(self.spec.shutdown_timeout_ms);
        let start: Instant = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait()? {
                return if status.success() {
                    Ok(StopOutcome::Graceful)
                }
                else {
                    Err(io::Error::other(format!("shutdown exited with {status}")))
                };
            }
            let remaining: Duration = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                eprintln!("{}: shutdown timed out, terminating process", self.name);
                self.child.kill()?;
                self.child.wait()?;
                return Ok(StopOutcome::Forced);
            }
            thread::sleep(remaining.min(POLL_INTERVAL));
        }
    }

}
