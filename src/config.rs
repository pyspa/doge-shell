use crate::shell::APP_NAME;
use anyhow::Context as _;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
pub struct Completion {
    pub target: String,
    pub completion_cmd: String,
    pub post_processing: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub alias: HashMap<String, String>,
    pub completions: Vec<Completion>,
    pub wasm: Option<String>, // wasm dir
}

impl Default for Config {
    fn default() -> Config {
        let alias: HashMap<String, String> = HashMap::new();
        let completions = Vec::new();
        let xdg_dir =
            xdg::BaseDirectories::with_prefix(APP_NAME).expect("failed get xdg directory");
        let wasm = xdg_dir
            .place_config_file("wasm")
            .expect("failed get path")
            .to_string_lossy()
            .to_string();

        Config {
            alias,
            completions,
            wasm: Some(wasm),
        }
    }
}

impl Config {
    fn read_file(name: &str) -> Result<Self> {
        let xdg_dir =
            xdg::BaseDirectories::with_prefix(APP_NAME).context("failed get xdg directory")?;
        let file_path = xdg_dir.place_config_file(name).context("failed get path")?;
        let toml_str: String = std::fs::read_to_string(file_path)?;

        let config: Config = toml::from_str(&toml_str)?;
        Ok(config)
    }

    pub fn from_file(name: &str) -> Self {
        match Config::read_file(name) {
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
        let mut completions = Vec::new();
        alias.insert("ll".to_string(), "ls -al".to_string());
        alias.insert("g".to_string(), "git".to_string());

        let compl = Completion {
            target: "a".to_string(),
            completion_cmd: "b".to_string(),
            post_processing: Some("c".to_string()),
        };
        completions.push(compl);
        let compl = Completion {
            target: "d".to_string(),
            completion_cmd: "e".to_string(),
            post_processing: Some("f".to_string()),
        };
        completions.push(compl);

        let config = Config { alias, completions };
        let toml_str = toml::to_string(&config)?;
        println!("{}", toml_str);

        let config: Config = toml::from_str(&toml_str)?;
        let val = config.alias.get("ll");
        assert_eq!(val, Some(&"ls -al".to_string()));
        Ok(())
    }
}
