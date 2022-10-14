use crate::shell::APP_NAME;
use anyhow::Context as _;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub alias: HashMap<String, String>,
}

fn default_vec_str() -> Vec<String> {
    Vec::new()
}
fn default_bool() -> bool {
    false
}
fn default_num() -> u64 {
    1
}
fn default_zero() -> u64 {
    0
}

impl Default for Config {
    fn default() -> Config {
        let alias: HashMap<String, String> = HashMap::new();
        Config { alias }
    }
}

impl Config {
    fn read_file(name: &str) -> Result<Self> {
        let xdg_dir =
            xdg::BaseDirectories::with_prefix(APP_NAME).context("failed get xdg directory")?;
        let file_path = xdg_dir.place_data_file(name).context("failed get path")?;
        let toml_str: String = std::fs::read_to_string(file_path)?;

        let config: Config = toml::from_str(&toml_str)?;
        Ok(config)
    }

    pub fn from_file(name: &str) -> Self {
        match Config::read_file(&name) {
            Ok(conf) => conf,
            Err(_) => Config::default(),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn init() {
        let _ = env_logger::try_init();
    }

    #[test]
    fn parse_config() -> Result<()> {
        let mut alias: HashMap<String, String> = HashMap::new();
        alias.insert("ll".to_string(), "ls -al".to_string());
        alias.insert("g".to_string(), "git".to_string());
        let config = Config { alias };
        let toml_str = toml::to_string(&config)?;

        let config: Config = toml::from_str(&toml_str)?;
        let val = config.alias.get("ll");
        assert_eq!(val, Some(&"ls -al".to_string()));
        Ok(())
    }
}
