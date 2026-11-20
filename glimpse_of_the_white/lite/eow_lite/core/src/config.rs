use std::{collections::{HashMap, HashSet}, fs, io, path::{Component, Path, PathBuf}, sync::Arc};

use eow_lite_helper::ModuleRegistry;
use serde::Deserialize;

use crate::splitter::SplitterConfig;

pub const CONFIG_MARKER: &str = ".eow_lite_config_root";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreConfig {
    pub schema: String,
    #[serde(rename = "LLM_Model")]
    pub model: ModelConfig,
    #[serde(rename = "LLM_Prompt")]
    pub prompt: PromptConfig,
    #[serde(rename = "SentenceSplitter")]
    pub splitter: SplitterConfig,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    pub url: String,
    pub model: String,
    pub api_key: String,
    #[serde(default)]
    pub stream: bool,
    pub temperature: Option<String>,
    pub thinking: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptConfig {
    pub module_block_order: Vec<String>,
    pub module_block_separator: String,
    pub core_block_order: Vec<PromptFile>,
    pub core_block_separator: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptFile {
    pub file: FileReference,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReference {
    pub root: String,
    pub path: PathBuf,
}

pub struct LoadedConfig {
    pub model: ModelConfig,
    pub prompt: PromptConfig,
    pub core_prompt: String,
    pub splitter: Arc<SplitterConfig>,
}

impl CoreConfig {

    pub fn load(directory: &Path, registry: &ModuleRegistry) -> io::Result<LoadedConfig> {
        let directory: PathBuf = directory.canonicalize()?;
        let root: PathBuf = directory
            .parent()
            .ok_or_else(|| invalid("module directory has no parent"))?
            .join("config");
        if !root.join(CONFIG_MARKER).is_file() {
            return Err(invalid(format!(
                "missing configuration marker in {}",
                root.display()
            )));
        }

        let mut files: HashMap<PathBuf, PathBuf> = HashMap::new();
        collect_files(&root, &root, &mut files)?;
        let core_path: &PathBuf = files
            .get(Path::new("core.toml"))
            .ok_or_else(|| invalid("missing runtime config/core.toml"))?;
        let content: String = fs::read_to_string(core_path)?;
        let mut config: Self = toml::from_str(&content).map_err(|error: toml::de::Error| {
            let line: usize = error.span().map_or(1, |span: std::ops::Range<usize>| {
                content[..span.start]
                    .bytes()
                    .filter(|byte: &u8| *byte == b'\n')
                    .count()
                    + 1
            });
            let detail: &str = if error.message().starts_with("unknown field ") {
                error.message()
            }
            else {
                "invalid configuration fields"
            };
            invalid(format!("invalid core.toml at line {line}: {detail}"))
        })?;
        if config.schema != "1.1" {
            return Err(invalid("core.toml schema must be 1.1"));
        }
        let mut modules: HashSet<&String> = HashSet::new();
        for module in &config.prompt.module_block_order {
            if registry.get(module).is_none() || !modules.insert(module) {
                return Err(invalid(format!(
                    "invalid or duplicate prompt module {module}"
                )));
            }
        }

        let mut blocks: Vec<String> = Vec::new();
        for reference in &config.prompt.core_block_order {
            let file: &FileReference = &reference.file;
            if file.root != CONFIG_MARKER
                || file.path.is_absolute()
                || file.path
                    .components()
                    .any(|part: Component<'_>| !matches!(part, Component::Normal(_) | Component::CurDir))
                || file.path
                    .extension()
                    .is_none_or(|extension: &std::ffi::OsStr| extension != "md")
            {
                return Err(invalid("invalid Core prompt file reference"));
            }
            let normalized: PathBuf = file.path
                .components()
                .filter_map(|part: Component<'_>| match part {
                    Component::Normal(name) => Some(name),
                    _ => None,
                })
                .collect();
            let path: &PathBuf = files
                .get(&normalized)
                .ok_or_else(|| invalid(format!("missing prompt file {}", normalized.display())))?;
            blocks.push(fs::read_to_string(path)?);
        }
        let core_prompt: String = blocks.join(&config.prompt.core_block_separator);
        config.splitter.compile()?;
        Ok(LoadedConfig {
            model: config.model,
            prompt: config.prompt,
            core_prompt,
            splitter: Arc::new(config.splitter),
        })
    }

}

fn collect_files(root: &Path, directory: &Path, files: &mut HashMap<PathBuf, PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry: std::fs::DirEntry = entry?;
        let kind: std::fs::FileType = entry.file_type()?;
        let path: PathBuf = entry.path();
        if kind.is_dir() {
            collect_files(root, &path, files)?;
        }
        else if kind.is_file()
            && path
                .extension()
                .is_some_and(|ext: &std::ffi::OsStr| matches!(ext.to_str(), Some("toml" | "json" | "md")))
        {
            files.insert(
                path.strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_owned(),
                path,
            );
        }
    }
    Ok(())
}

pub fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
