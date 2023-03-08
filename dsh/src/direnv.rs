use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

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
                let cfg = read_config_file(file).unwrap();
                for data in &cfg {
                    self.entries.push(EnvEntry {
                        key: data[0].to_string(),
                        value: data[1].to_string(),
                    });
                }
            }
        }
    }
}

fn read_config_file(file: &str) -> Result<Vec<[String; 2]>> {
    let mut ret: Vec<[String; 2]> = Vec::new();
    let contents = fs::read_to_string(file)?;
    for line in contents.lines() {
        let parts: Vec<&str> = line.splitn(2, '=').collect();
        let key = parts[0].trim().to_uppercase().to_string();
        let value = parts[1].trim().to_string();
        ret.push([key, value]);
    }
    Ok(ret)
}

pub fn check_path(pwd: &Path, entries: &mut Vec<DirEnvironment>) {
    for env in entries {
        if pwd.starts_with(&env.path) {
            if !env.loaded {
                env.read_envfile();
                println!("direnv: loading {}", env.path);
                print!("direnv: export ");
                env.set_env();
                println!("");
                env.loaded = true;
            }
            env.set_env();
        } else if env.loaded {
            println!("direnv: unloading {}", env.path);
            print!("direnv: unxport ");
            env.remove_env();
            println!("");
            env.loaded = false;
        }
    }
}
