use crate::ai_features::AiService;
use crate::completion::AutoComplete;
use crate::direnv::DirEnvironment;
use crate::dirs::search_file;
use crate::shell::APP_NAME;
use crate::suggestion::{InputPreferences, SuggestionMode};
use anyhow::Context as _;
use anyhow::Result;
use dsh_builtin::McpManager;
use dsh_types::mcp::McpServerConfig;
use dsh_types::output_history::{self, OutputHistory};
use parking_lot::RwLock;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// Pre-compiled regex patterns for path processing
static ABSOLUTE_PATH_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^/").unwrap());
static RELATIVE_PATH_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^\.\/").unwrap());

use tracing::debug;

pub trait ChangePwdHook: Send + Sync {
    fn call(&self, pwd: &Path, env: Arc<RwLock<Environment>>) -> Result<()>;
}

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
    pub execute_allowlist: Vec<String>,
    pub system_env_vars: HashMap<String, String>,
    pub input_preferences: InputPreferences,
    pub safety_level: Arc<RwLock<crate::safety::SafetyLevel>>,
    /// Cache for PATH command lookups to avoid repeated filesystem access
    command_cache: RwLock<HashMap<String, Option<String>>>,
    /// Prewarmed list of executable names from PATH directories for fast prefix search
    pub executable_names: Arc<RwLock<Vec<String>>>,
    /// Output history for $OUT[N] and $ERR[N] variables
    pub output_history: OutputHistory,
    pub ai_service: Option<Arc<dyn AiService + Send + Sync>>,
    // Z command exclusion patterns
    pub z_exclude: Vec<String>,
    /// Flags if the shell is currently in startup mode (e.g. running config.lisp)
    pub startup_mode: bool,
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
            execute_allowlist: Vec::new(),
            system_env_vars: env::vars().collect(),
            input_preferences: default_input_preferences(),
            safety_level: Arc::new(RwLock::new(crate::safety::SafetyLevel::Normal)),

            command_cache: RwLock::new(HashMap::new()),
            executable_names: Arc::new(RwLock::new(Vec::new())),
            output_history: OutputHistory::new(),
            ai_service: None,
            z_exclude: parse_z_exclude(),
            startup_mode: false,
        }));

        {
            let mut env = env_arc.write();
            env.variables
                .insert("SAFETY_LEVEL".to_string(), "normal".to_string());
        }

        env_arc
    }

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

        // Check cache first for PATH lookups
        {
            if let Some(cached) = self.command_cache.read().get(cmd) {
                return cached.clone();
            }
        }

        // Cache miss: search PATH directories
        let result = self.lookup_path_uncached(cmd);

        // Update cache
        self.command_cache
            .write()
            .insert(cmd.to_string(), result.clone());
        result
    }

    /// Lookup command with cache update (mutable version for cache population)
    /// Note: With the new interior mutability, this is functionally the same as lookup
    pub fn lookup_cached(&mut self, cmd: &str) -> Option<String> {
        self.lookup(cmd)
    }

    fn lookup_path_uncached(&self, cmd: &str) -> Option<String> {
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
        // Clear command cache when PATH changes
        self.command_cache.write().clear();
        // Also clear executable names cache
        self.executable_names.write().clear();
    }

    pub fn reload_z_exclude(&mut self) {
        self.z_exclude = parse_z_exclude();
    }

    /// Clear the command lookup cache
    pub fn clear_command_cache(&mut self) {
        self.command_cache.get_mut().clear();
    }

    /// Prewarm the executable names cache by scanning PATH directories.
    /// This should be called in the background after shell startup.
    pub fn prewarm_executables(&self) {
        use std::fs::read_dir;
        use std::os::unix::fs::PermissionsExt;

        let mut names = HashSet::new();
        for path in &self.paths {
            if let Ok(entries) = read_dir(path) {
                for entry in entries.flatten() {
                    if let Ok(ft) = entry.file_type()
                        && (ft.is_file() || ft.is_symlink())
                            && let Ok(meta) = entry.metadata()
                                && meta.permissions().mode() & 0o111 != 0
                                    && let Some(name) = entry.file_name().to_str() {
                                        names.insert(name.to_string());
                                    }
                }
            }
        }

        let mut sorted: Vec<String> = names.into_iter().collect();
        sorted.sort();
        *self.executable_names.write() = sorted;
        debug!(
            "Prewarmed {} executable names",
            self.executable_names.read().len()
        );
    }

    /// Set the prewarmed executable names (called after background collection).
    pub fn set_executable_names(&mut self, names: Vec<String>) {
        debug!("Setting {} prewarmed executable names", names.len());
        *self.executable_names.write() = names;
    }

    /// Search for an executable name by prefix using the prewarmed cache.
    /// Returns the first matching executable name, or None if not found.
    pub fn search_prefix(&self, prefix: &str) -> Option<String> {
        let names = self.executable_names.read();
        if names.is_empty() {
            // Cache not prewarmed yet, fall back to synchronous search
            return self.search(prefix);
        }

        // Binary search for the first name >= prefix
        let start = names.partition_point(|n| n.as_str() < prefix);
        if start < names.len() && names[start].starts_with(prefix) {
            return Some(names[start].clone());
        }
        None
    }

    pub fn get_var(&self, key: &str) -> Option<String> {
        // Check $OUT[N] and $ERR[N] patterns first
        if let Some(index) = output_history::parse_output_var(key, "OUT") {
            return self.output_history.get_stdout(index).map(|s| s.to_string());
        }
        if let Some(index) = output_history::parse_output_var(key, "ERR") {
            return self.output_history.get_stderr(index).map(|s| s.to_string());
        }

        // Check MCP-related dynamic variables
        match key {
            "MCP_SERVERS" => {
                return Some(self.mcp_manager.read().server_count().to_string());
            }
            "MCP_CONNECTED" => {
                return Some(self.mcp_manager.read().connected_count().to_string());
            }
            "MCP_TOOLS" => {
                return Some(self.mcp_manager.read().tool_count().to_string());
            }
            _ => {}
        }

        let val = self.variables.get(key);
        if val.is_some() {
            return val.map(|x| x.to_string());
        }

        if let Some(var) = key.strip_prefix('$') {
            // expand env var
            self.system_env_vars.get(var).cloned()
        } else {
            // For compatibility, also check OS env vars without the '$' prefix
            self.system_env_vars.get(key).cloned()
        }
    }
    pub fn clear_mcp_servers(&mut self) {
        self.mcp_servers.clear();
    }

    pub fn add_mcp_server(&mut self, server: McpServerConfig) {
        // In startup mode, we only register the server config but don't connect yet.
        // The actual connection happens asynchronously via reload_mcp_config() later.
        if !self.startup_mode {
            // Try to add to the active manager first (synchronously blocking)
            if let Err(e) = self.mcp_manager.write().add_server_blocking(server.clone()) {
                eprintln!("Failed to register MCP server: {}", e);
            }
        }
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

    pub fn suggestion_mode(&self) -> SuggestionMode {
        self.input_preferences.suggestion_mode
    }

    pub fn set_suggestion_mode(&mut self, mode: SuggestionMode) {
        self.input_preferences.suggestion_mode = mode;
    }

    pub fn suggestion_ai_enabled(&self) -> bool {
        self.input_preferences.ai_backfill
    }

    pub fn set_suggestion_ai_enabled(&mut self, enabled: bool) {
        self.input_preferences.ai_backfill = enabled;
    }

    pub fn set_auto_fix_enabled(&mut self, enabled: bool) {
        self.input_preferences.auto_fix = enabled;
    }

    pub fn input_preferences(&self) -> InputPreferences {
        self.input_preferences
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
            .field("exported_vars", &self.exported_vars)
            .field("mcp_servers", &self.mcp_servers)
            .field("execute_allowlist", &self.execute_allowlist)
            .field("input_preferences", &self.input_preferences)
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
                                && let Some(name) = entry.file_name().to_str() {
                                    names.insert(name.to_string());
                                }
            }
        }
    }

    let mut sorted: Vec<String> = names.into_iter().collect();
    sorted.sort();
    sorted
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

        assert_eq!(2, env1.read().variables.len());
    }

    #[test]
    fn test_resolve_alias() {
        init();
        let env = Environment::new();
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

    #[test]
    fn auto_enables_ai_backfill_when_api_key_present() {
        init();

        let key = "AI_CHAT_API_KEY";
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, "test-key");
        }

        let prefs = default_input_preferences();
        assert!(
            prefs.ai_backfill,
            "AI suggestions should auto-enable when key is set"
        );

        if let Some(value) = previous {
            unsafe {
                std::env::set_var(key, value);
            }
        } else {
            unsafe {
                std::env::remove_var(key);
            }
        }
    }
    #[test]
    fn test_search() {
        init();
        let env = Environment::new();
        // Test absolute path
        let abs_path = "/usr/bin/env";
        if Path::new(abs_path).exists() {
            let p = env.read().search(abs_path);
            assert_eq!(Some(abs_path.to_string()), p);
        }

        // Test relative path (assumes running from repo root with Cargo.toml)
        let rel_path = "./Cargo.toml";
        if Path::new(rel_path).exists() {
            let p = env.read().search(rel_path);
            assert_eq!(Some(rel_path.to_string()), p);
        }

        // Test non-existent path
        let non_existent = "./non_existent_file_12345";
        let p = env.read().search(non_existent);
        assert_eq!(None, p);

        // Test command in PATH
        let p = env.read().search("ls");
        // Should find ls in one of the paths, usually /usr/bin/ls or /bin/ls
        // Note: search() via search_file() returns just the filename for PATH lookups
        assert!(p.is_some());
        assert_eq!(p.unwrap(), "ls");
    }
}
