mod control;
mod plan;
mod platform;
mod process;

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use eow_lite_helper::ModuleRegistry;

use plan::{ExitPolicy, Plan};
use platform::ProcessContainer;
use process::{ManagedProcess, POLL_INTERVAL};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("eow-lite-supervisor: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let command = args.next();
    let command = command
        .as_deref()
        .and_then(|arg| arg.to_str())
        .filter(|command| matches!(*command, "link" | "unlink"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "expected link or unlink"))?;
    let directory = match args.next() {
        Some(directory) => PathBuf::from(directory),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
    };
    if args.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unexpected Supervisor argument",
        ));
    }
    let plan = Plan::load(&directory)?;
    let port = plan.port()?;
    match command {
        "link" => link(&directory, &plan, port),
        "unlink" => {
            let timeout = plan
                .modules
                .iter()
                .fold(Duration::from_secs(3), |total, spec| {
                    total.saturating_add(Duration::from_millis(spec.shutdown_timeout_ms))
                });
            control::unlink(port, timeout)?;
            println!("EOW Lite stopped");
            Ok(())
        }
        _ => unreachable!(),
    }
}

fn link(directory: &Path, plan: &Plan, port: u16) -> io::Result<()> {
    let stopping = Arc::new(AtomicBool::new(false));
    {
        let stopping = Arc::clone(&stopping);
        ctrlc::set_handler(move || stopping.store(true, Ordering::Release))
            .map_err(io::Error::other)?;
    }
    let registry = ModuleRegistry::load(directory)?;
    let executable = std::env::current_exe()?;
    let bin = executable
        .parent()
        .expect("Supervisor executable has a parent directory");
    let launches = plan
        .modules
        .iter()
        .map(|spec| {
            let module = registry.get(&spec.module).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is missing from the module registry", spec.module),
                )
            })?;
            let path = bin.join(format!(
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

    let container = ProcessContainer::new()?;
    let control = control::Control::bind(port, Arc::clone(&stopping))?;
    println!("EOW Lite Supervisor Started");
    println!("PID: {}", std::process::id());
    println!("Control: 127.0.0.1:{port}");
    println!();

    let mut children = Vec::new();
    let run = (|| {
        for (spec, name, path) in launches {
            if stopping.load(Ordering::Acquire) {
                return Ok(());
            }
            check_children(&mut children)?;
            println!("STARTING {name}");
            let start = Instant::now();
            let child =
                ManagedProcess::spawn(spec, name, &path, directory, &container).map_err(|err| {
                    io::Error::new(err.kind(), format!("could not start {name}: {err}"))
                })?;
            children.push(child);
            children.last_mut().unwrap().bootstrap()?;
            let timeout = Duration::from_millis(spec.startup_timeout_ms);
            loop {
                if stopping.load(Ordering::Acquire) {
                    return Ok(());
                }
                check_children(&mut children)?;
                let remaining = timeout.saturating_sub(start.elapsed());
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
                    .map_err(|err| io::Error::new(err.kind(), format!("{name}: {err}")))?
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

    let shutdown_requested = run.is_ok();
    if shutdown_requested {
        println!();
        println!("Shutdown requested");
        println!();
    }

    let mut clean = true;
    for child in children.iter_mut().rev() {
        match child.stop() {
            Ok(outcome) if shutdown_requested => {
                println!("STOPPED   {:<11} {outcome:?}", child.name);
            }
            Ok(_) => {}
            Err(err) => {
                clean = false;
                eprintln!("{}: {err}", child.name);
            }
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
                }
            }
        }
    }
    Ok(())
}
