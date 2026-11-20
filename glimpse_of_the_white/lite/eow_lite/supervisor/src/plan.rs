use std::{collections::HashSet, fs, io, path::Path};

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Plan {
    schema: String,
    supervisor_port: String,
    pub modules: Vec<ModuleSpec>,
}

#[derive(Deserialize)]
pub struct ModuleSpec {
    pub module: String,
    pub executable: String,
    pub startup_timeout_ms: u64,
    pub shutdown_timeout_ms: u64,
    pub unexpected_exit_policy: ExitPolicy,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitPolicy {
    Shutdown,
}

impl Plan {

    pub fn load(directory: &Path) -> io::Result<Self> {
        let path: std::path::PathBuf = directory.join("lifecycle.json");
        let plan: Self = serde_json::from_slice(&fs::read(&path)?).map_err(|err: serde_json::Error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {err}", path.display()),
            )
        })?;
        if plan.schema != "1.0" || plan.modules.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid lifecycle schema or empty module list",
            ));
        }
        let mut names: HashSet<&String> = HashSet::new();
        for module in &plan.modules {
            if !names.insert(&module.module)
                || module.executable.is_empty()
                || !module.executable
                    .bytes()
                    .all(|b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                || module.startup_timeout_ms == 0
                || module.shutdown_timeout_ms == 0
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid lifecycle entry for {:?}", module.module),
                ));
            }
        }
        Ok(plan)
    }

    pub fn port(&self) -> io::Result<u16> {
        self.supervisor_port
            .parse::<u16>()
            .ok()
            .filter(|port: &u16| *port != 0)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid lifecycle supervisor_port",
                )
            })
    }

}
