use crate::environment::Environment;
use anyhow::Result;
use parking_lot::RwLock;
use std::fs;
use std::io::{BufWriter, StdoutLock, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum Entry {
    Env(EnvEntry),
    PathAdd(PathAddEntry),
}

#[derive(Debug, Clone)]
pub struct PathAddEntry {
    pub path: String,
    pub old: String,
}

#[derive(Debug, Clone)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct DirEnvironment {
    pub path: String,
    pub entries: Vec<Entry>,
    pub env_path: String,
    loaded: bool,
}

impl DirEnvironment {
    pub fn new(path: String) -> Result<Self> {
        let env_path = std::env::var("PATH")?;
        Ok(DirEnvironment {
            path,
            entries: Vec::new(),
            env_path,
            loaded: false,
        })
    }

    pub fn set_env(&self, out: &mut BufWriter<StdoutLock>) -> Result<()> {
        if self.loaded {
            return Ok(());
        }

        let mut env_path = std::env::var("PATH")?;
        for entry in &self.entries {
            match entry {
                Entry::Env(env_entry) => {
                    unsafe { std::env::set_var(&env_entry.key, &env_entry.value) };
                    out.write_fmt(format_args!("+{} ", &env_entry.key)).ok();
                    // print!("+{} ", &env_entry.key);
                }
                Entry::PathAdd(path_entry) => {
                    let mut path = path_entry.path.clone();
                    path.push_str(&env_path);
                    env_path = path;
                }
            }
        }
        unsafe { std::env::set_var("PATH", &env_path) };

        Ok(())
    }

    pub fn remove_env(&mut self) {
        if !self.loaded {
            return;
        }
        let mut require_reset = false;
        for entry in &self.entries {
            match entry {
                Entry::Env(env_entry) => {
                    unsafe { std::env::remove_var(&env_entry.key) };
                }
                Entry::PathAdd(_) => {
                    require_reset = true;
                }
            }
        }
        if require_reset {
            unsafe { std::env::set_var("PATH", &self.env_path) };
        }
        self.entries = Vec::new();
    }

    pub fn read_env_file(&mut self) -> Result<()> {
        if self.loaded {
            return Ok(());
        }

        let root = PathBuf::from(&self.path);
        let env_file = root.join(".env");
        let envrc_file = root.join(".envrc");
        if env_file.exists() {
            if let Some(file) = env_file.to_str() {
                let cfgs = read_env_config_file(file)?;
                for data in cfgs {
                    self.entries.push(data);
                }
            }
        } else if envrc_file.exists()
            && let Some(file) = envrc_file.to_str() {
                let cfgs = read_envrc_config_file(file)?;
                for data in cfgs {
                    self.entries.push(data);
                }
            }
        Ok(())
    }
}

fn read_env_config_file(file: &str) -> Result<Vec<Entry>> {
    let mut ret: Vec<Entry> = Vec::new();
    let contents = fs::read_to_string(file)?;
    for line in contents.lines() {
        let parts: Vec<&str> = line.splitn(2, '=').collect();
        let key = parts[0].trim().to_uppercase().to_string();
        let value = parts[1].trim().to_string();
        ret.push(Entry::Env(EnvEntry { key, value }));
    }
    Ok(ret)
}

fn read_envrc_config_file(file: &str) -> Result<Vec<Entry>> {
    let mut ret: Vec<Entry> = Vec::new();
    let contents = fs::read_to_string(file)?;
    let current_path = std::env::var("PATH")?;

    for line in contents.lines() {
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        let cmd = parts[0].trim().to_uppercase().to_string();
        let value = parts[1].trim().to_string();

        match cmd.as_str() {
            "PATH_ADD" => ret.push(Entry::PathAdd(PathAddEntry {
                path: value,
                old: current_path.clone(),
            })),
            "EXPORT" => {
                let parts: Vec<&str> = value.splitn(2, '=').collect();
                let key = parts[0].trim().to_uppercase().to_string();
                let value = parts[1].trim().to_string();
                ret.push(Entry::Env(EnvEntry { key, value }));
            }
            _ => {}
        }
    }
    Ok(ret)
}

pub fn check_path(pwd: &Path, environment: Arc<RwLock<Environment>>) -> Result<()> {
    let environment = &mut environment.write();
    let entries = &mut environment.direnv_roots;
    let out = std::io::stdout().lock();
    let mut out = BufWriter::new(out);

    for env in entries {
        if pwd.starts_with(&env.path) {
            if !env.loaded {
                env.read_env_file()?;
                out.write_fmt(format_args!("direnv: loading {}\n", env.path))
                    .ok();
                out.write_all(b"direnv: export ").ok();
                env.set_env(&mut out)?;
                out.write_all(b"\n").ok();
                env.loaded = true;
            }
            // env.set_env();
        } else if env.loaded {
            out.write_fmt(format_args!("direnv: unloading {}\n", env.path))
                .ok();
            env.remove_env();
            env.loaded = false;
        }
    }
    out.flush().ok();
    environment.reload_path();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dir_environment_creation() -> Result<()> {
        let path = "/tmp/test".to_string();

        let dir_env = DirEnvironment::new(path.clone())?;
        assert_eq!(dir_env.path, path);
        assert!(dir_env.entries.is_empty());

        Ok(())
    }

    #[test]
    fn test_dir_environment_basic_functionality() -> Result<()> {
        let dir_env = DirEnvironment::new("/tmp/test".to_string())?;

        // Basic functionality test
        assert_eq!(dir_env.path, "/tmp/test");
        assert!(dir_env.entries.is_empty());

        Ok(())
    }
}
