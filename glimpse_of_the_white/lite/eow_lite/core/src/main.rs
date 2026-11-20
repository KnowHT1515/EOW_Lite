mod config;
mod context;
mod http;
mod prompt;
mod protocol;
mod runtime;
mod splitter;

use std::{io, path::PathBuf, process::ExitCode, sync::Arc, time::Duration};

use eow_lite_helper::Binding;
use eow_lite_helper::lifecycle::{ModuleLifecycle, announce};
use tokio::sync::mpsc;

use crate::config::CoreConfig;
use crate::runtime::Runtime;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("eow-lite-core: {error}");
            ExitCode::FAILURE
        },
    }
}

async fn run() -> io::Result<()> {
    let lifecycle: ModuleLifecycle = ModuleLifecycle::load(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."))?;
    let name: String = lifecycle.module_name("core")?.to_owned();
    let config: crate::config::LoadedConfig = CoreConfig::load(lifecycle.directory(), &lifecycle.registry)?;
    let (registry, shutdown): (eow_lite_helper::ModuleRegistry, _) = lifecycle.into_parts();
    let registry: Arc<eow_lite_helper::ModuleRegistry> = Arc::new(registry);
    let binding: Binding = Binding::connect(Arc::clone(&registry), "core", Duration::from_secs(10))
        .await
        .map_err(io::Error::other)?;
    let mut runtime: Runtime = Runtime::new(binding, registry, config)?;
    let (signal, mut signals): (mpsc::UnboundedSender<()>, mpsc::UnboundedReceiver<()>) = mpsc::unbounded_channel();
    ctrlc::set_handler(move || {
        let _ = signal.send(());
    })
    .map_err(io::Error::other)?;
    announce(&name, "Online")?;
    let result: io::Result<()> = tokio::select! {
        result = shutdown => result,
        _ = signals.recv() => Ok(()),
        result = runtime.run() => result,
    };
    runtime.stop().await;
    result?;
    announce(&name, "Offline")
}
