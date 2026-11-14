use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct ModuleEntry {
    module: String,
    module_name: String,
    #[serde(rename = "MODULE_NAME")]
    module_name_upper: String,
    number: String,
}

impl ModuleEntry {
    fn valid(&self) -> bool {
        valid_name(&self.module)
            && self.module.bytes().all(|b| !b.is_ascii_uppercase())
            && valid_name(&self.module_name)
            && valid_name(&self.module_name_upper)
            && self
                .module_name_upper
                .bytes()
                .all(|b| !b.is_ascii_lowercase())
            && self.number.len() == 2
            && self.number.bytes().all(|b| b.is_ascii_digit())
    }

    fn fields(&self) -> [&str; 4] {
        [
            &self.module,
            &self.module_name,
            &self.module_name_upper,
            &self.number,
        ]
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_')
}

#[derive(Debug, Deserialize)]
struct PortEntry {
    module: String,
    port: String,
}

#[derive(Debug)]
pub struct EowLiteModule {
    entry: ModuleEntry,
    port: String,
}

impl EowLiteModule {
    pub fn module(&self) -> &str {
        &self.entry.module
    }

    pub fn module_name(&self) -> &str {
        &self.entry.module_name
    }

    pub fn module_name_upper(&self) -> &str {
        &self.entry.module_name_upper
    }

    pub fn number(&self) -> &str {
        &self.entry.number
    }

    pub fn port(&self) -> &str {
        &self.port
    }
}

#[derive(Debug)]
pub struct ModuleRegistry {
    modules: Vec<EowLiteModule>,
}

impl ModuleRegistry {
    pub fn load(directory: impl AsRef<Path>) -> io::Result<Self> {
        let directory = directory.as_ref();
        let entries: Vec<ModuleEntry> = load_array(&directory.join("module.json"))?
            .into_iter()
            .filter_map(|value| serde_json::from_value::<ModuleEntry>(value).ok())
            .filter(ModuleEntry::valid)
            .collect();
        let mut counts: [_; 4] = std::array::from_fn(|_| HashMap::new());
        for entry in &entries {
            for (count, field) in counts.iter_mut().zip(entry.fields()) {
                *count.entry(field).or_insert(0usize) += 1;
            }
        }

        let mut ports: HashMap<String, Option<String>> = HashMap::new();
        for entry in load_array(&directory.join("ports.json"))?
            .into_iter()
            .filter_map(|value| serde_json::from_value::<PortEntry>(value).ok())
        {
            ports
                .entry(entry.module)
                .and_modify(|port| *port = None)
                .or_insert(Some(entry.port));
        }

        let unique: Vec<bool> = entries
            .iter()
            .map(|entry| {
                counts
                    .iter()
                    .zip(entry.fields())
                    .all(|(count, field)| count[field] == 1)
            })
            .collect();
        let modules = entries
            .into_iter()
            .zip(unique)
            .filter_map(|(entry, unique)| {
                if !unique {
                    return None;
                }

                let port = ports.remove(&entry.module).flatten()?;
                Some(EowLiteModule { entry, port })
            })
            .collect();
        Ok(Self { modules })
    }

    pub fn modules(&self) -> &[EowLiteModule] {
        &self.modules
    }

    pub fn get(&self, module: &str) -> Option<&EowLiteModule> {
        self.modules.iter().find(|entry| entry.module() == module)
    }
}

fn load_array(path: &Path) -> io::Result<Vec<Value>> {
    let bytes = fs::read(path)
        .map_err(|err| io::Error::new(err.kind(), format!("{}: {err}", path.display())))?;
    serde_json::from_slice(&bytes).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {err}", path.display()),
        )
    })
}
