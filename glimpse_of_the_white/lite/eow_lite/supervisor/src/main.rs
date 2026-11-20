mod control;
mod plan;
mod platform;
mod process;

use std::{io, path::{Path, PathBuf}, process::ExitCode, sync::{Arc, atomic::{AtomicBool, Ordering}}, thread, time::{Duration, Instant}};

use eow_lite_helper::ModuleRegistry;

use crate::plan::{ExitPolicy, Plan};
use crate::platform::ProcessContainer;
use crate::process::{ManagedProcess, POLL_INTERVAL};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("eow-lite-supervisor: {err}");
            ExitCode::FAILURE
        },
    }
}

fn run() -> io::Result<()> {
    let mut args: std::iter::Skip<std::env::ArgsOs> = std::env::args_os().skip(1);
    let command: Option<std::ffi::OsString> = args.next();
    let command: &str = command
        .as_deref()
        .and_then(|arg: &std::ffi::OsStr| arg.to_str())
        .filter(|command: &&str| matches!(*command, "link" | "unlink"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "expected link or unlink"))?;
    let directory: PathBuf = match args.next() {
        Some(directory) => PathBuf::from(directory),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
    };
    if args.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unexpected Supervisor argument",
        ));
    }
    let plan: Plan = Plan::load(&directory)?;
    let port: u16 = plan.port()?;
    match command {
        "link" => link(&directory, &plan, port),
        "unlink" => {
            let timeout: Duration = plan.modules
                .iter()
                .fold(Duration::from_secs(3), |total: Duration, spec: &crate::plan::ModuleSpec| {
                    total.saturating_add(Duration::from_millis(spec.shutdown_timeout_ms))
                });
            control::unlink(port, timeout)?;
            println!("EOW Lite stopped");
            Ok(())
        },
        _ => unreachable!(),
    }
}

fn link(directory: &Path, plan: &Plan, port: u16) -> io::Result<()> {
    let stopping: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    {
        let stopping: Arc<AtomicBool> = Arc::clone(&stopping);
        ctrlc::set_handler(move || stopping.store(true, Ordering::Release))
            .map_err(io::Error::other)?;
    }
    let registry: ModuleRegistry = ModuleRegistry::load(directory)?;
    let executable: PathBuf = std::env::current_exe()?;
    let bin: &Path = executable
        .parent()
        .expect("Supervisor executable has a parent directory");
    let launches: Vec<(&crate::plan::ModuleSpec, &str, PathBuf)> = plan.modules
        .iter()
        .map(|spec: &crate::plan::ModuleSpec| {
            let module: &eow_lite_helper::EowLiteModule = registry.get(&spec.module).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is missing from the module registry", spec.module),
                )
            })?;
            let path: PathBuf = bin.join(format!(
                "{}{}",
                spec.executable,
                std::env::consts::EXE_SUFFIX
            ));
            if !path.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("module executable not found: {}", path.display()),
                ));
            }
            Ok((spec, module.module_name(), path))
        })
        .collect::<io::Result<Vec<_>>>()?;

    let container: ProcessContainer = ProcessContainer::new()?;
    let control: crate::control::Control = control::Control::bind(port, Arc::clone(&stopping))?;
    println!("EOW Lite Supervisor Started");
    println!("PID: {}", std::process::id());
    println!("Control: 127.0.0.1:{port}");
    println!();

    let mut children: Vec<ManagedProcess<'_>> = Vec::new();
    let run: io::Result<()> = (|| {
        for (spec, name, path) in launches {
            if stopping.load(Ordering::Acquire) {
                return Ok(());
            }
            check_children(&mut children)?;
            println!("STARTING {name}");
            let start: Instant = Instant::now();
            let child: ManagedProcess<'_> =
                ManagedProcess::spawn(spec, name, &path, directory, &container).map_err(|err: io::Error| {
                    io::Error::new(err.kind(), format!("could not start {name}: {err}"))
                })?;
            children.push(child);
            children.last_mut().unwrap().bootstrap()?;
            let timeout: Duration = Duration::from_millis(spec.startup_timeout_ms);
            loop {
                if stopping.load(Ordering::Acquire) {
                    return Ok(());
                }
                check_children(&mut children)?;
                let remaining: Duration = timeout.saturating_sub(start.elapsed());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("timed out waiting for {name} Online"),
                    ));
                }
                if children
                    .last()
                    .unwrap()
                    .online()
                    .map_err(|err: io::Error| io::Error::new(err.kind(), format!("{name}: {err}")))?
                {
                    break;
                }
                thread::sleep(remaining.min(POLL_INTERVAL));
            }
        }
        if stopping.load(Ordering::Acquire) {
            return Ok(());
        }
        check_children(&mut children)?;
        print_online_report(&children);

        while !stopping.load(Ordering::Acquire) {
            check_children(&mut children)?;
            thread::sleep(POLL_INTERVAL);
        }
        Ok(())
    })();

    let shutdown_requested: bool = run.is_ok();
    if shutdown_requested {
        println!();
        println!("Shutdown requested");
        println!();
    }

    let mut clean: bool = true;
    for child in children.iter_mut().rev() {
        match child.stop() {
            Ok(outcome) if shutdown_requested => {
                println!("STOPPED   {:<11} {outcome:?}", child.name);
            },
            Ok(_) => {},
            Err(err) => {
                clean = false;
                eprintln!("{}: {err}", child.name);
            },
        }
    }
    drop(container);
    if shutdown_requested && clean {
        println!();
        println!("EOW Lite Offline");
    }
    control.finish(clean);
    run?;
    if !clean {
        return Err(io::Error::other(
            "one or more modules failed to shut down cleanly",
        ));
    }
    Ok(())
}

fn print_online_report(children: &[ManagedProcess<'_>]) {
    println!();
    println!("EOW Lite Online");
    println!();
    for child in children {
        println!("ONLINE   {:<11} pid={}", child.name, child.id());
    }
    println!();
    println!("Shutdown: eow-lite unlink");
}

fn check_children(children: &mut [ManagedProcess<'_>]) -> io::Result<()> {
    for child in children {
        if let Some(status) = child.try_wait()? {
            match child.spec.unexpected_exit_policy {
                ExitPolicy::Shutdown => {
                    return Err(io::Error::other(format!(
                        "{} exited unexpectedly with {status}",
                        child.name
                    )));
                },
            }
        }
    }
    Ok(())
}
