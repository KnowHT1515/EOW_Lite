use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    eow_lite_helper::lifecycle::run_idle(
        "adapter",
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
    )
}
