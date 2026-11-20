use std::{collections::HashMap, io};

use crate::config::{PromptConfig, invalid};

pub struct Prompt {
    config: PromptConfig,
    core: String,
    modules: HashMap<String, String>,
}

impl Prompt {

    pub fn new(config: PromptConfig, core: String) -> Self {
        Self {
            config,
            core,
            modules: HashMap::new(),
        }
    }

    pub fn push(&mut self, source: &str, module: String, prompt: String) -> io::Result<()> {
        if source != module || module == "core" || !self.config.module_block_order.contains(&module)
        {
            return Err(invalid(
                "prompt module must match its source and be configured in module_block_order",
            ));
        }
        self.modules.insert(module, prompt);
        Ok(())
    }

    pub fn snapshot(&self) -> String {
        self.config.module_block_order
            .iter()
            .filter_map(|module: &String| {
                if module == "core" {
                    Some(self.core.as_str())
                }
                else {
                    self.modules.get(module).map(String::as_str)
                }
            })
            .collect::<Vec<_>>()
            .join(&self.config.module_block_separator)
    }

}
