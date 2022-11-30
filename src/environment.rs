use crate::config::Config;
use crate::dirs::search_file;
use crate::script;
use crate::wasm;
use log::debug;
use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::{cell::RefCell, rc::Rc};

#[derive(Debug)]
pub struct Environment {
    pub config: Rc<RefCell<Config>>,
    paths: Vec<String>,
    variables: HashMap<String, String>,
    pub wasm_engine: wasm::WasmEngine,
}

impl Environment {
    pub fn new() -> Self {
        let mut paths = ["/bin", "/usr/bin", "/sbin", "/usr/sbin"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        if let Ok(val) = env::var("PATH") {
            paths = val.split(':').map(|s| s.to_string()).collect();
        }

        debug!("default path {:?}", &paths);

        let config = Rc::new(RefCell::new(Config::default()));
        if let Err(err) = script::read_config_lisp(config.clone()) {
            eprintln!("failed load init lisp {:?}", err);
        }
        let wasm_engine = if let Some(wasm_dir) = &config.borrow().wasm {
            wasm::WasmEngine::from_path(wasm_dir)
        } else {
            Default::default()
        };

        Environment {
            variables: HashMap::new(),
            paths,
            config,
            wasm_engine,
        }
    }

    pub fn lookup(&self, cmd: &str) -> Option<String> {
        if cmd.starts_with('/') {
            let cmd_path = Path::new(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return Some(cmd.to_string());
            } else {
                return None;
            }
        }
        if cmd.starts_with("./") {
            let cmd_path = Path::new(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return Some(cmd.to_string());
            } else {
                return None;
            }
        }
        for path in &self.paths {
            let cmd_path = Path::new(path).join(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return cmd_path.to_str().map(|s| s.to_string());
            }
        }
        None
    }

    pub fn search(&self, cmd: &str) -> Option<String> {
        if cmd.starts_with('/') {
            // TODO
        }
        if cmd.starts_with("./") {
            // TODO
        }
        for path in &self.paths {
            if let Some(file) = search_file(path, cmd) {
                return Some(file);
            }
        }
        None
    }
}

impl Default for Environment {
    fn default() -> Self {
        Environment::new()
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
    fn test_lookup() {
        let env = Environment::default();
        let p = env.lookup("touch");
        assert_eq!(Some("/usr/bin/touch".to_string()), p)
    }
}
