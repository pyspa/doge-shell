//! Environment module for shell configuration and state.
//!
//! This module provides the core `Environment` struct that holds:
//! - PATH and command lookup
//! - Variables and exports
//! - Aliases and abbreviations
//! - MCP server configuration
//! - Input preferences
//!
//! # Module Structure
//!
//! - [`paths`] - PATH lookup and command caching
//! - [`variables`] - Variable and alias resolution  
//! - [`mcp`] - MCP server management
//! - [`preferences`] - Input preferences and settings

mod mcp;
mod paths;
mod preferences;
mod variables;

#[cfg(test)]
mod tests;

use crate::ai_features::AiService;
use crate::completion::AutoComplete;
use crate::direnv::DirEnvironment;
use crate::secrets::SecretManager;
use crate::shell::APP_NAME;
use crate::suggestion::InputPreferences;
use anyhow::Context as _;
use anyhow::Result;
use dsh_builtin::McpManager;
use dsh_types::mcp::McpServerConfig;
use dsh_types::output_history::OutputHistory;
use parking_lot::RwLock;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use tracing::debug;

// Pre-compiled regex patterns for path processing
pub(crate) static ABSOLUTE_PATH_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^/").unwrap());
pub(crate) static RELATIVE_PATH_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^\./").unwrap());

/// Hook called when the current directory changes.
pub trait ChangePwdHook: Send + Sync {
    fn call(&self, pwd: &std::path::Path, env: Arc<RwLock<Environment>>) -> Result<()>;
}

/// Shell environment configuration and state.
pub struct Environment {
    pub alias: HashMap<String, String>,
    pub abbreviations: HashMap<String, String>,
    pub autocompletion: Vec<AutoComplete>,
    pub paths: Vec<String>,
    pub variables: HashMap<String, String>,
    pub exported_vars: HashSet<String>,
    pub direnv_roots: Vec<DirEnvironment>,
    pub chpwd_hooks: Vec<Box<dyn ChangePwdHook + Send + Sync>>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub mcp_manager: Arc<RwLock<McpManager>>,
    pub execute_allowlist: Arc<RwLock<Vec<String>>>,
    pub system_env_vars: HashMap<String, String>,
    pub input_preferences: InputPreferences,
    pub safety_level: Arc<RwLock<crate::safety::SafetyLevel>>,
    /// Cache for PATH command lookups to avoid repeated filesystem access
    pub(crate) command_cache: RwLock<HashMap<String, Option<String>>>,
    /// Prewarmed list of executable names from PATH directories for fast prefix search
    pub executable_names: Arc<RwLock<Vec<String>>>,
    /// Output history for $OUT[N] and $ERR[N] variables
    pub output_history: OutputHistory,
    pub ai_service: Option<Arc<dyn AiService + Send + Sync>>,
    /// Z command exclusion patterns
    pub z_exclude: Vec<String>,
    /// Flags if the shell is currently in startup mode (e.g. running config.lisp)
    pub startup_mode: bool,
    /// Secret manager for handling sensitive information
    pub secret_manager: SecretManager,
}

fn default_input_preferences() -> InputPreferences {
    let mut prefs = InputPreferences::default();
    if ai_credentials_available() {
        prefs.ai_backfill = true;
    }
    prefs
}

fn ai_credentials_available() -> bool {
    env_has_nonempty("AI_CHAT_API_KEY")
        || env_has_nonempty("OPENAI_API_KEY")
        || env_has_nonempty("OPEN_AI_API_KEY")
}

fn env_has_nonempty(key: &str) -> bool {
    matches!(env::var(key), Ok(value) if !value.trim().is_empty())
}

fn parse_z_exclude() -> Vec<String> {
    if let Ok(val) = env::var("Z_EXCLUDE") {
        val.split(':').map(|s| s.to_string()).collect()
    } else {
        Vec::new()
    }
}

impl Environment {
    /// Create a new environment with default settings.
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
        let env_arc = Arc::new(RwLock::new(Environment {
            alias: HashMap::new(),
            abbreviations: HashMap::new(),
            autocompletion: Vec::new(),
            variables: HashMap::new(),
            exported_vars: HashSet::new(),
            paths,
            direnv_roots: Vec::new(),
            chpwd_hooks: Vec::new(),
            mcp_servers: Vec::new(),
            mcp_manager: Arc::new(RwLock::new(McpManager::default())),
            execute_allowlist: Arc::new(RwLock::new(Vec::new())),
            system_env_vars: env::vars().collect(),
            input_preferences: default_input_preferences(),
            safety_level: Arc::new(RwLock::new(crate::safety::SafetyLevel::Normal)),

            command_cache: RwLock::new(HashMap::new()),
            executable_names: Arc::new(RwLock::new(Vec::new())),
            output_history: OutputHistory::new(),
            ai_service: None,
            z_exclude: parse_z_exclude(),
            startup_mode: false,
            secret_manager: SecretManager::new(),
        }));

        {
            let mut env = env_arc.write();
            env.variables
                .insert("SAFETY_LEVEL".to_string(), "normal".to_string());
        }

        env_arc
    }

    /// Create a child environment that inherits from the parent.
    pub fn extend(parent: Arc<RwLock<Environment>>) -> Arc<RwLock<Self>> {
        let alias = parent.read().alias.clone();
        let abbreviations = parent.read().abbreviations.clone();
        let paths = parent.read().paths.clone();
        let autocompletion = parent.read().autocompletion.clone();
        let variables = parent.read().variables.clone();
        let exported_vars = parent.read().exported_vars.clone();
        let direnv_roots = parent.read().direnv_roots.clone();
        let mcp_servers = parent.read().mcp_servers.clone();
        let mcp_manager = parent.read().mcp_manager.clone();
        let execute_allowlist = parent.read().execute_allowlist.clone();
        let input_preferences = parent.read().input_preferences;
        let system_env_vars = parent.read().system_env_vars.clone();
        let safety_level = parent.read().safety_level.clone();

        #[allow(clippy::arc_with_non_send_sync)]
        Arc::new(RwLock::new(Environment {
            alias,
            abbreviations,
            autocompletion,
            variables,
            exported_vars,
            paths,
            direnv_roots,
            chpwd_hooks: Vec::new(),
            mcp_servers,
            mcp_manager,
            execute_allowlist,
            system_env_vars,
            input_preferences,
            safety_level,
            command_cache: RwLock::new(HashMap::new()),
            executable_names: Arc::new(RwLock::new(Vec::new())),
            output_history: OutputHistory::new(),
            ai_service: parent.read().ai_service.clone(),
            z_exclude: parent.read().z_exclude.clone(),
            startup_mode: false, // Extended environments (subshells) are not in startup mode
            secret_manager: SecretManager::new(),
        }))
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
            .field("exported_vars", &self.exported_vars)
            .field("mcp_servers", &self.mcp_servers)
            .field("execute_allowlist", &self.execute_allowlist)
            .field("input_preferences", &self.input_preferences)
            .finish()
    }
}

/// Get the path to a configuration file.
pub fn get_config_file(name: &str) -> Result<PathBuf> {
    let xdg_dir =
        xdg::BaseDirectories::with_prefix(APP_NAME).context("failed get xdg directory")?;
    xdg_dir.place_config_file(name).context("failed get path")
}

/// Get the path to a data file.
pub fn get_data_file(name: &str) -> Result<PathBuf> {
    #[cfg(test)]
    ensure_test_data_dir();
    let xdg_dir =
        xdg::BaseDirectories::with_prefix(APP_NAME).context("failed get xdg directory")?;
    xdg_dir.place_data_file(name).context("failed get path")
}

/// Get the path to a state file (e.g. logs).
pub fn get_state_file(name: &str) -> Result<PathBuf> {
    #[cfg(test)]
    ensure_test_data_dir();
    let xdg_dir =
        xdg::BaseDirectories::with_prefix(APP_NAME).context("failed get xdg directory")?;
    xdg_dir.place_state_file(name).context("failed get path")
}

#[cfg(test)]
fn ensure_test_data_dir() {
    use std::sync::OnceLock;

    if std::env::var_os("XDG_DATA_HOME").is_some() {
        return;
    }

    static TEST_DATA_DIR: OnceLock<PathBuf> = OnceLock::new();
    let dir = TEST_DATA_DIR.get_or_init(|| {
        let base = std::env::temp_dir().join(format!("dsh-test-data-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&base);
        base
    });

    unsafe {
        std::env::set_var("XDG_DATA_HOME", dir);
    }
}

/// Collect executable names from the given PATH directories.
/// This is a standalone function to allow calling from a background thread.
pub fn collect_executables(paths: &[String]) -> Vec<String> {
    use std::fs::read_dir;
    use std::os::unix::fs::PermissionsExt;

    let mut names = HashSet::new();
    for path in paths {
        if let Ok(entries) = read_dir(path) {
            for entry in entries.flatten() {
                if let Ok(ft) = entry.file_type()
                    && (ft.is_file() || ft.is_symlink())
                    && let Ok(meta) = entry.metadata()
                    && meta.permissions().mode() & 0o111 != 0
                    && let Some(name) = entry.file_name().to_str()
                {
                    names.insert(name.to_string());
                }
            }
        }
    }

    let mut sorted: Vec<String> = names.into_iter().collect();
    sorted.sort();
    sorted
}
