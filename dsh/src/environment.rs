use crate::completion::AutoComplete;
use crate::direnv::DirEnvironment;
use crate::dirs::search_file;
use crate::shell::APP_NAME;
use anyhow::Context as _;
use anyhow::Result;
use dsh_types::mcp::McpServerConfig;
use parking_lot::RwLock;
use regex::Regex;
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

// Pre-compiled regex patterns for path processing
static ABSOLUTE_PATH_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^/").unwrap());
static RELATIVE_PATH_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^\.\/").unwrap());
#[allow(dead_code)]
static HOME_PATH_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^~").unwrap());

/// Environment change notification mechanism
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct EnvironmentVersion {
    version: Arc<AtomicU64>,
}

#[allow(dead_code)]
impl EnvironmentVersion {
    pub fn new() -> Self {
        Self {
            version: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn increment(&self) {
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }
}

use tracing::debug;

pub trait ChangePwdHook {
    fn call(&self, pwd: &Path, env: Arc<RwLock<Environment>>) -> Result<()>;
}

pub struct Environment {
    pub alias: HashMap<String, String>,
    pub abbreviations: HashMap<String, String>,
    pub autocompletion: Vec<AutoComplete>,
    pub paths: Vec<String>,
    pub variables: HashMap<String, String>,
    pub direnv_roots: Vec<DirEnvironment>,
    pub chpwd_hooks: Vec<Box<dyn ChangePwdHook>>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub execute_allowlist: Vec<String>,
}

impl Environment {
    pub fn new() -> Arc<RwLock<Self>> {
        let mut paths = ["/bin", "/usr/bin", "/sbin", "/usr/sbin"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        if let Ok(val) = env::var("PATH") {
            paths = val.split(':').map(|s| s.to_string()).collect();
        }

        debug!("default path {:?}", &paths);

        #[allow(clippy::arc_with_non_send_sync)]
        Arc::new(RwLock::new(Environment {
            alias: HashMap::new(),
            abbreviations: HashMap::new(),
            autocompletion: Vec::new(),
            variables: HashMap::new(),
            paths,
            direnv_roots: Vec::new(),
            chpwd_hooks: Vec::new(),
            mcp_servers: Vec::new(),
            execute_allowlist: Vec::new(),
        }))
    }

    pub fn extend(parent: Arc<RwLock<Environment>>) -> Arc<RwLock<Self>> {
        let alias = parent.read().alias.clone();
        let abbreviations = parent.read().abbreviations.clone();
        let paths = parent.read().paths.clone();
        let autocompletion = parent.read().autocompletion.clone();
        let variables = parent.read().variables.clone();
        let direnv_roots = parent.read().direnv_roots.clone();
        let mcp_servers = parent.read().mcp_servers.clone();
        let execute_allowlist = parent.read().execute_allowlist.clone();

        #[allow(clippy::arc_with_non_send_sync)]
        Arc::new(RwLock::new(Environment {
            alias,
            abbreviations,
            autocompletion,
            variables,
            paths,
            direnv_roots,
            chpwd_hooks: Vec::new(),
            mcp_servers,
            execute_allowlist,
        }))
    }

    pub fn lookup(&self, cmd: &str) -> Option<String> {
        if ABSOLUTE_PATH_REGEX.is_match(cmd) {
            let cmd_path = Path::new(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return Some(cmd.to_string());
            } else {
                return None;
            }
        }
        if RELATIVE_PATH_REGEX.is_match(cmd) {
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
        if ABSOLUTE_PATH_REGEX.is_match(cmd) {
            // TODO
        }
        if RELATIVE_PATH_REGEX.is_match(cmd) {
            // TODO
        }
        for path in &self.paths {
            if let Some(file) = search_file(path, cmd) {
                return Some(file);
            }
        }
        None
    }

    pub fn reload_path(&mut self) {
        let mut paths: Vec<String> = ["/bin", "/usr/bin", "/sbin", "/usr/sbin"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        if let Ok(val) = env::var("PATH") {
            paths = val.split(':').map(|s| s.to_string()).collect();
        }
        self.paths = paths;
    }

    pub fn get_var(&self, key: &str) -> Option<String> {
        let val = self.variables.get(key);
        if val.is_some() {
            return val.map(|x| x.to_string());
        }

        if let Some(var) = key.strip_prefix('$') {
            // expand env var
            std::env::var(var).ok()
        } else {
            None
        }
    }
    pub fn clear_mcp_servers(&mut self) {
        self.mcp_servers.clear();
    }

    pub fn add_mcp_server(&mut self, server: McpServerConfig) {
        self.mcp_servers.push(server);
    }

    pub fn mcp_servers(&self) -> &[McpServerConfig] {
        &self.mcp_servers
    }

    pub fn clear_execute_allowlist(&mut self) {
        self.execute_allowlist.clear();
    }

    pub fn add_execute_allowlist_entry(&mut self, entry: String) {
        if !self.execute_allowlist.contains(&entry) {
            self.execute_allowlist.push(entry);
        }
    }

    pub fn execute_allowlist(&self) -> &[String] {
        &self.execute_allowlist
    }

    /// Resolves an alias from the Environment's alias map.
    /// If the name is an alias, returns the expanded command; otherwise, returns the original name.
    pub fn resolve_alias(&self, name: &str) -> String {
        self.alias
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }
}

impl std::fmt::Debug for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("Environment")
            .field("alias", &self.alias)
            .field("abbreviations", &self.abbreviations)
            .field("autocompletion", &self.autocompletion)
            .field("direnv_paths", &self.direnv_roots)
            .field("paths", &self.paths)
            .field("variables", &self.variables)
            .field("mcp_servers", &self.mcp_servers)
            .field("execute_allowlist", &self.execute_allowlist)
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
mod tests {
    use super::*;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn test_lookup() {
        init();
        let env = Environment::new();
        let p = env.read().lookup("touch");
        assert_eq!(Some("/usr/bin/touch".to_string()), p)
    }

    #[test]
    fn test_extend() {
        init();
        let env = Environment::new();
        let env1 = Arc::clone(&env);
        env.write()
            .variables
            .insert("test".to_string(), "value".to_string());

        let env2 = Environment::extend(env);
        let env2_clone = Arc::clone(&env2);

        env2.write()
            .variables
            .insert("test2".to_string(), "value2".to_string());

        let env2_clone = env2_clone.read();
        let v = env2_clone.variables.get("test");
        assert_eq!("value".to_string(), *v.unwrap());
        assert_eq!(
            "value2".to_string(),
            *env2_clone.variables.get("test2").unwrap()
        );

        assert_eq!(1, env1.read().variables.len());
    }

    #[test]
    fn test_resolve_alias() {
        init();
        let mut env = Environment::new();
        env.write()
            .alias
            .insert("ll".to_string(), "ls -la".to_string());

        // Test alias resolution
        let resolved = env.read().resolve_alias("ll");
        assert_eq!(resolved, "ls -la".to_string());

        // Test non-alias fallback
        let resolved = env.read().resolve_alias("unknown");
        assert_eq!(resolved, "unknown".to_string());
    }
}
