use crate::completion::AutoComplete;
use crate::dirs::search_file;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::{cell::RefCell, rc::Rc};
use tracing::debug;

#[derive(Debug)]
pub struct Environment {
    pub alias: HashMap<String, String>,
    pub autocompletion: Vec<AutoComplete>,
    paths: Vec<String>,
    pub variables: HashMap<String, String>,
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

        let alias: HashMap<String, String> = HashMap::new();
        Rc::new(RefCell::new(Environment {
            alias,
            autocompletion: Vec::new(),
            variables: HashMap::new(),
            paths,
        }))
    }

    pub fn extend(parent: Rc<RefCell<Environment>>) -> Rc<RefCell<Self>> {
        let alias = parent.borrow().alias.clone();
        let paths = parent.borrow().paths.clone();
        let autocompletion = parent.borrow().autocompletion.clone();
        let variables = parent.borrow().variables.clone();

        Rc::new(RefCell::new(Environment {
            alias,
            autocompletion,
            variables,
            paths,
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
