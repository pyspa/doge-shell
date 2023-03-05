use crate::completion::AutoComplete;
use crate::direnv::DirEnvironment;
use crate::dirs::search_file;
use crate::shell::APP_NAME;
use anyhow::Context as _;
use anyhow::Result;
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::{cell::RefCell, rc::Rc};
use tracing::debug;

pub struct Environment {
    pub alias: HashMap<String, String>,
    pub autocompletion: Vec<AutoComplete>,
    pub paths: Vec<String>,
    pub variables: HashMap<String, String>,
    pub direnv_roots: Vec<DirEnvironment>,
}

impl Environment {
    pub fn new() -> Rc<RefCell<Self>> {
        let mut paths = ["/bin", "/usr/bin", "/sbin", "/usr/sbin"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        if let Ok(val) = env::var("PATH") {
            paths = val.split(':').map(|s| s.to_string()).collect();
        }

        debug!("default path {:?}", &paths);

        Rc::new(RefCell::new(Environment {
            alias: HashMap::new(),
            autocompletion: Vec::new(),
            variables: HashMap::new(),
            paths,
            direnv_roots: Vec::new(),
        }))
    }

    pub fn extend(parent: Rc<RefCell<Environment>>) -> Rc<RefCell<Self>> {
        let alias = parent.borrow().alias.clone();
        let paths = parent.borrow().paths.clone();
        let autocompletion = parent.borrow().autocompletion.clone();
        let variables = parent.borrow().variables.clone();
        let direnv_roots = parent.borrow().direnv_roots.clone();

        Rc::new(RefCell::new(Environment {
            alias,
            autocompletion,
            variables,
            paths,
            direnv_roots,
        }))
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

impl std::fmt::Debug for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("Environment")
            .field("alias", &self.alias)
            .field("autocompletion", &self.autocompletion)
            .field("direnv_paths", &self.direnv_roots)
            .field("paths", &self.paths)
            .field("variables", &self.variables)
            .finish()
    }
}

pub fn get_config_file(name: &str) -> Result<PathBuf> {
    let xdg_dir =
        xdg::BaseDirectories::with_prefix(APP_NAME).context("failed get xdg directory")?;
    xdg_dir.place_config_file(name).context("failed get path")
}

pub fn get_data_file(name: &str) -> Result<PathBuf> {
    let xdg_dir =
        xdg::BaseDirectories::with_prefix(APP_NAME).context("failed get xdg directory")?;
    xdg_dir.place_data_file(name).context("failed get path")
}

#[cfg(test)]
mod test {
    use super::*;

    fn init() {
        tracing_subscriber::fmt::init();
    }

    #[test]
    fn test_lookup() {
        let env = Environment::new();
        let p = env.borrow().lookup("touch");
        assert_eq!(Some("/usr/bin/touch".to_string()), p)
    }

    #[test]
    fn test_extend() {
        let env = Environment::new();
        let env1 = Rc::clone(&env);
        env.borrow_mut()
            .variables
            .insert("test".to_string(), "value".to_string());

        let env2 = Environment::extend(env);
        let env2_clone = Rc::clone(&env2);

        env2.borrow_mut()
            .variables
            .insert("test2".to_string(), "value2".to_string());

        let env2_clone = env2_clone.borrow();
        let v = env2_clone.variables.get("test");
        assert_eq!("value".to_string(), *v.unwrap());
        assert_eq!(
            "value2".to_string(),
            *env2_clone.variables.get("test2").unwrap()
        );

        assert_eq!(1, env1.borrow().variables.len());
    }
}
