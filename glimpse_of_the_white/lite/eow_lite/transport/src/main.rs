mod hub;

use std::io;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::ExitCode;

use eow_lite_helper::lifecycle::{ModuleLifecycle, announce};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("eow-lite-transport: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> io::Result<()> {
    let lifecycle = ModuleLifecycle::load(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."))?;
    let name = lifecycle.module_name("transport")?.to_owned();
    let module = lifecycle
        .registry
        .get("transport")
        .expect("module was checked above");
    let port = module
        .port()
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid transport port {:?}", module.port()),
            )
        })?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    if !lifecycle.is_supervised() {
        eprintln!("{name} listening on {}", listener.local_addr()?);
    }
    announce(&name, "Online")?;
    let (registry, shutdown) = lifecycle.into_parts();
    tokio::select! {
        result = hub::serve(listener, registry) => result,
        result = shutdown => {
            result?;
            announce(&name, "Offline")
        },
    }
}
