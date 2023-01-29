use crate::environment::Environment;
use simple_config_parser::Config;
use std::path::{Path, PathBuf};
use std::{cell::RefCell, rc::Rc};

#[derive(Debug, Clone)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct DirEnvironment {
    pub path: String,
    pub entries: Vec<EnvEntry>,
    loaded: bool,
}

impl DirEnvironment {
    pub fn new(path: String) -> Self {
        DirEnvironment {
            path,
            entries: Vec::new(),
            loaded: false,
        }
    }

    pub fn set_env(&self) {
        if self.loaded {
            return;
        }
        for entry in &self.entries {
            std::env::set_var(&entry.key, &entry.value);
            print!("+{} ", &entry.key);
        }
    }

    pub fn remove_env(&self) {
        if !self.loaded {
            return;
        }
        for entry in &self.entries {
            std::env::remove_var(&entry.key);
            print!("-{} ", &entry.key);
        }
    }

    pub fn read_envfile(&mut self) {
        if self.loaded {
            return;
        }
        let root = PathBuf::from(&self.path);
        let env_file = root.join(".env");
        if env_file.exists() {
            if let Some(file) = env_file.to_str() {
                let cfg = Config::new().file(file).unwrap();
                for data in &cfg.data {
                    self.entries.push(EnvEntry {
                        key: data[0].to_string().to_uppercase(),
                        value: data[1].to_string(),
                    });
                }
            }
        }
    }
}

pub fn check_path(pwd: &Path, env: Rc<RefCell<Environment>>) {
    for env in env.borrow_mut().direnv_roots.iter_mut() {
        if pwd.starts_with(&env.path) {
            if !env.loaded {
                env.read_envfile();
                print!("load direnv ");
                env.set_env();
                println!("");
                env.loaded = true;
            }
            env.set_env();
        } else {
            if env.loaded {
                print!("unload direnv ");
                env.remove_env();
                println!("");
                env.loaded = false;
            }
        }
    }
}
