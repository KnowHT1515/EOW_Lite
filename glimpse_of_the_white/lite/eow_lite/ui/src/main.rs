#![cfg_attr(windows, windows_subsystem = "windows")]

mod ipc;
mod launch;
mod protocol;
mod runtime;

use std::{io, process::ExitCode};

use crate::launch::Session;
use crate::protocol::ServerMessage;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("eow-lite-ui: {error}");
            ExitCode::FAILURE
        },
    }
}

async fn run() -> io::Result<()> {
    let mut session: Session = Session::open().await?;
    let result: io::Result<()> = runtime::run(&mut session).await;
    if let Err(error) = &result {
        let _ = session.send(&ServerMessage::Notice { message: error.to_string() }).await;
    }
    session.close().await;
    result
}
