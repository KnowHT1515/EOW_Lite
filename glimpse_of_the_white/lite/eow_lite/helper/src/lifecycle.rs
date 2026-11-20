use std::{io::{self, BufRead, Write}, path::PathBuf, process::ExitCode};

use tokio::sync::oneshot;

use crate::ModuleRegistry;

pub const BOOTSTRAP: &str = "bootstrap";
pub const SHUTDOWN: &str = "shutdown";

pub struct ModuleLifecycle {
    pub registry: ModuleRegistry,
    shutdown: Option<oneshot::Receiver<io::Result<()>>>,
}

impl ModuleLifecycle {

    pub fn load(default_directory: PathBuf) -> io::Result<Self> {
        let mut args: std::iter::Skip<std::env::ArgsOs> = std::env::args_os().skip(1);
        let first: Option<std::ffi::OsString> = args.next();
        let supervised: bool = first.as_deref().is_some_and(|arg: &std::ffi::OsStr| arg == "supervised");
        let directory: PathBuf = if supervised {
            args.next().map(PathBuf::from).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "missing configuration directory",
                )
            })?
        }
        else {
            first.map(PathBuf::from).unwrap_or(default_directory)
        };
        if args.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unexpected module argument",
            ));
        }

        let shutdown: Option<oneshot::Receiver<io::Result<()>>> = if supervised {
            let stdin: io::Stdin = io::stdin();
            let mut line: String = String::new();
            stdin.read_line(&mut line)?;
            if line.trim_end() != BOOTSTRAP {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "missing bootstrap command",
                ));
            }
            let (sender, receiver): (oneshot::Sender<io::Result<()>>, oneshot::Receiver<io::Result<()>>) = oneshot::channel();
            std::thread::spawn(move || {
                let _ = sender.send(read_shutdown(stdin.lock()));
            });
            Some(receiver)
        }
        else {
            None
        };
        Ok(Self {
            registry: ModuleRegistry::load(directory)?,
            shutdown,
        })
    }

    pub fn module_name(&self, module: &str) -> io::Result<&str> {
        self.registry
            .get(module)
            .map(|entry: &crate::registry::EowLiteModule| entry.module_name())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{module} is missing from the module registry"),
                )
            })
    }

    pub fn is_supervised(&self) -> bool {
        self.shutdown.is_some()
    }

    pub fn into_parts(self) -> (ModuleRegistry, impl Future<Output = io::Result<()>>) {
        let shutdown: Option<oneshot::Receiver<io::Result<()>>> = self.shutdown;
        (self.registry, async move {
            match shutdown {
                Some(receiver) => receiver.await.map_err(io::Error::other)?,
                None => std::future::pending().await,
            }
        })
    }

}

fn read_shutdown(stdin: impl BufRead) -> io::Result<()> {
    for line in stdin.lines() {
        if line?.trim_end() == SHUTDOWN {
            break;
        }
    }
    Ok(())
}

pub fn announce(module_name: &str, state: &str) -> io::Result<()> {
    let mut out: io::StdoutLock<'_> = io::stdout().lock();
    writeln!(out, "{module_name} {state}")?;
    out.flush()
}

pub fn run_idle(module: &str, default_directory: PathBuf) -> ExitCode {
    let run = || {
        let mut lifecycle: ModuleLifecycle = ModuleLifecycle::load(default_directory)?;
        let name: String = lifecycle.module_name(module)?.to_owned();
        announce(&name, "Online")?;
        if let Some(shutdown) = lifecycle.shutdown.take() {
            shutdown.blocking_recv().map_err(io::Error::other)??;
            announce(&name, "Offline")
        }
        else {
            loop {
                std::thread::park();
            }
        }
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("eow-lite-{module}: {err}");
            ExitCode::FAILURE
        },
    }
}
