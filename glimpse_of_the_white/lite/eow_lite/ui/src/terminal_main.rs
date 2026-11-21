mod ipc;
mod protocol;
mod render;
mod terminal;
mod windows_input;

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    match terminal::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if ipc::disconnected(&error) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("eow-lite-ui-terminal: {error}");
            ExitCode::FAILURE
        },
    }
}
