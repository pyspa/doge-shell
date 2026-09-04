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
use crate::direnv::DirEnvironment;
use crate::history::CommandLedgerMode;
use crate::secrets::SecretManager;
use crate::shell::APP_NAME;
use crate::suggestion::InputPreferences;
use anyhow::Context as _;
use anyhow::Result;
use dsh_builtin::McpManager;
use dsh_types::command_block::CommandBlockHistory;
use dsh_types::mcp::McpServerConfig;
use dsh_types::output_history::OutputHistory;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use tracing::debug;

const EXECUTABLE_CACHE_FILE: &str = "executable_names.json";

/// Hook called when the current directory changes.
pub trait ChangePwdHook: Send + Sync {
    fn call(&self, pwd: &std::path::Path, env: Arc<RwLock<Environment>>) -> Result<()>;
}

/// Shell environment configuration and state.
pub(crate) struct VariableState {
    pub(crate) alias: HashMap<String, String>,
    pub(crate) abbreviations: HashMap<String, String>,
    pub(crate) command_abbreviations: HashMap<String, HashMap<String, String>>,
    pub(crate) command_ledger_mode: CommandLedgerMode,
    pub(crate) paths: Vec<String>,
    pub(crate) variables: HashMap<String, String>,
    pub(crate) exported_vars: HashSet<String>,
    pub(crate) direnv_roots: Vec<DirEnvironment>,
    pub(crate) chpwd_hooks: Vec<Box<dyn ChangePwdHook + Send + Sync>>,
    pub(crate) system_env_vars: HashMap<String, String>,
    pub(crate) z_exclude: Vec<String>,
    /// User key bindings from `config.lisp`, layered over the built-in table.
    ///
    /// Lives here rather than in `Repl` because `config.lisp` runs before the
    /// REPL exists, and alongside `abbreviations` because it is configuration
    /// with the same lifecycle — including being rolled back by
    /// `EnvironmentSnapshot` when the config fails to load.
    pub(crate) keybindings: crate::repl::keybind::KeyBindings,
}

pub(crate) struct PolicyState {
    /// Commands the operator configured, via `(chat-execute-add ...)` or the
    /// `execute` tool's JSON config. Both the shell's own safety check and the
    /// chat agent honour these.
    pub(crate) execute_allowlist: Arc<RwLock<Vec<String>>>,
    /// Commands the user waved through with "always" at an interactive safety
    /// prompt.
    ///
    /// Kept apart from `execute_allowlist` because that list is also the chat
    /// agent's permission set: while the two shared one store, approving your
    /// own `rm -rf build` at the prompt silently handed the same command to the
    /// AI for the rest of the session.
    pub(crate) shell_always_allowlist: Arc<RwLock<Vec<String>>>,
    /// Commands the user approved with "always" for the *agent*, this session.
    /// Never consulted for commands the user types.
    pub(crate) agent_session_allowlist: Arc<RwLock<Vec<String>>>,
    pub(crate) safety_level: Arc<RwLock<crate::safety::SafetyLevel>>,
    pub(crate) secret_manager: SecretManager,
}

pub(crate) struct IntegrationState {
    pub(crate) mcp_servers: Vec<McpServerConfig>,
    pub(crate) mcp_manager: Arc<RwLock<McpManager>>,
    /// `AI_MESSAGE_LANG`, shared with `LiveAiService`.
    ///
    /// A slot rather than a lookup because the service outlives any one borrow
    /// of the environment, and giving it the environment back would make a
    /// reference cycle - the environment holds the service.
    pub(crate) response_language: Arc<RwLock<Option<String>>>,
    pub(crate) ai_service: Option<Arc<dyn AiService + Send + Sync>>,
}

pub(crate) struct SessionOutputState {
    pub(crate) output_history: OutputHistory,
    pub(crate) command_blocks: CommandBlockHistory,
}

pub(crate) struct CompletionState {
    pub(crate) input_preferences: InputPreferences,
    pub(crate) command_cache: RwLock<HashMap<String, Option<String>>>,
    pub(crate) executable_names: Arc<RwLock<Vec<String>>>,
}

pub struct Environment {
    /// Exit status of the last command, for `$?`.
    ///
    /// Deliberately outside `variable_state`: this is runtime state, and
    /// `EnvironmentSnapshot` rolls back *configuration* only — restoring a
    /// stale exit status after a failed config reload would be wrong.
    pub(crate) last_exit_status: i32,
    pub(crate) variable_state: VariableState,
    pub(crate) policy_state: PolicyState,
    pub(crate) integration_state: IntegrationState,
    pub(crate) session_output_state: SessionOutputState,
    pub(crate) completion_state: CompletionState,
    /// The `pushd`/`popd` directory stack, bash-style: slot 0 is always the
    /// current directory.
    ///
    /// `changepwd` keeps slot 0 in sync, so plain `cd` replaces the top without
    /// disturbing what is underneath — and `dirs -v` numbering lines up with
    /// `cd -N`. Empty until the first directory change; `dirs` then falls back
    /// to `$PWD`.
    ///
    /// Deliberately outside `variable_state`: this is runtime navigation state,
    /// not configuration, so `EnvironmentSnapshot` must not roll it back when
    /// `config.lisp` fails.
    pub(crate) dir_stack: Vec<String>,
    /// Periodic tasks registered with `sched`.
    ///
    /// Shared with the background runner, which is why it is an `Arc` rather
    /// than a plain field. It lives on `Environment` (not on `Repl`) because
    /// `config.lisp` runs before the REPL is constructed and may register
    /// tasks. Like [`Environment::dir_stack`], it is runtime state and is not
    /// rolled back by `EnvironmentSnapshot`.
    pub(crate) scheduler: crate::scheduler::SharedScheduler,
    /// Flags if the shell is currently in startup mode (e.g. running config.lisp)
    pub(crate) startup_mode: bool,
}

fn parse_z_exclude_from_vars(vars: &HashMap<String, String>) -> Vec<String> {
    vars.get("Z_EXCLUDE")
        .map(|val| val.split(':').map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

impl Environment {
    /// Create a new environment with default settings.
    pub fn new() -> Arc<RwLock<Self>> {
        let system_env_vars: HashMap<String, String> = env::vars().collect();
        let z_exclude = parse_z_exclude_from_vars(&system_env_vars);
        let mut paths = ["/bin", "/usr/bin", "/sbin", "/usr/sbin"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        if let Some(val) = system_env_vars.get("PATH") {
            paths = val.split(':').map(|s| s.to_string()).collect();
        }

        debug!("default path {:?}", &paths);

        let env_arc = Arc::new(RwLock::new(Environment {
            last_exit_status: 0,
            variable_state: VariableState {
                alias: HashMap::new(),
                abbreviations: HashMap::new(),
                command_abbreviations: HashMap::new(),
                command_ledger_mode: CommandLedgerMode::Off,
                variables: HashMap::new(),
                exported_vars: HashSet::new(),
                paths,
                direnv_roots: Vec::new(),
                chpwd_hooks: Vec::new(),
                system_env_vars,
                z_exclude,
                keybindings: crate::repl::keybind::KeyBindings::with_defaults(),
            },
            policy_state: PolicyState {
                execute_allowlist: Arc::new(RwLock::new(Vec::new())),
                shell_always_allowlist: Arc::new(RwLock::new(Vec::new())),
                agent_session_allowlist: Arc::new(RwLock::new(Vec::new())),
                safety_level: Arc::new(RwLock::new(crate::safety::SafetyLevel::Normal)),
                secret_manager: SecretManager::new(),
            },
            integration_state: IntegrationState {
                mcp_servers: Vec::new(),
                mcp_manager: Arc::new(RwLock::new(McpManager::default())),
                response_language: Arc::new(RwLock::new(None)),
                ai_service: None,
            },
            session_output_state: SessionOutputState {
                output_history: OutputHistory::new(),
                command_blocks: CommandBlockHistory::new(),
            },
            completion_state: CompletionState {
                input_preferences: InputPreferences::default(),
                command_cache: RwLock::new(HashMap::new()),
                executable_names: Arc::new(RwLock::new(Vec::new())),
            },
            dir_stack: Vec::new(),
            scheduler: crate::scheduler::SchedulerState::shared(),
            startup_mode: false,
        }));

        {
            // Seed the level from the inherited environment before publishing the
            // variable. Writing "normal" unconditionally shadowed an inherited
            // `SAFETY_LEVEL=strict` in the shell variable map, so starting dsh
            // from a hardened parent shell silently dropped back to normal.
            let mut env = env_arc.write();
            let inherited = env
                .variable_state
                .system_env_vars
                .get("SAFETY_LEVEL")
                .cloned();
            let level = crate::safety::SafetyLevel::from_env_value(inherited);
            *env.policy_state.safety_level.write() = level;
            env.variable_state
                .variables
                .insert("SAFETY_LEVEL".to_string(), level.as_str().to_string());

            // Publish the inherited `AI_MESSAGE_LANG` once; after this the
            // variable setters keep the slot in step.
            env.reload_response_language();
        }

        env_arc
    }

    /// Create a child environment that inherits from the parent.
    pub fn extend(parent: Arc<RwLock<Environment>>) -> Arc<RwLock<Self>> {
        let variable_state = {
            let parent = parent.read();
            VariableState {
                alias: parent.variable_state.alias.clone(),
                abbreviations: parent.variable_state.abbreviations.clone(),
                command_abbreviations: parent.variable_state.command_abbreviations.clone(),
                command_ledger_mode: parent.variable_state.command_ledger_mode,
                paths: parent.variable_state.paths.clone(),
                variables: parent.variable_state.variables.clone(),
                exported_vars: parent.variable_state.exported_vars.clone(),
                direnv_roots: parent.variable_state.direnv_roots.clone(),
                chpwd_hooks: Vec::new(),
                system_env_vars: parent.variable_state.system_env_vars.clone(),
                z_exclude: parent.variable_state.z_exclude.clone(),
                keybindings: parent.variable_state.keybindings.clone(),
            }
        };
        let (integration_state, policy_state, completion_state) = {
            let parent = parent.read();
            (
                IntegrationState {
                    mcp_servers: parent.integration_state.mcp_servers.clone(),
                    mcp_manager: parent.integration_state.mcp_manager.clone(),
                    response_language: parent.integration_state.response_language.clone(),
                    ai_service: parent.integration_state.ai_service.clone(),
                },
                PolicyState {
                    execute_allowlist: parent.policy_state.execute_allowlist.clone(),
                    shell_always_allowlist: parent.policy_state.shell_always_allowlist.clone(),
                    agent_session_allowlist: parent.policy_state.agent_session_allowlist.clone(),
                    safety_level: parent.policy_state.safety_level.clone(),
                    secret_manager: SecretManager::new(),
                },
                CompletionState {
                    input_preferences: parent.completion_state.input_preferences,
                    command_cache: RwLock::new(HashMap::new()),
                    executable_names: Arc::new(RwLock::new(Vec::new())),
                },
            )
        };

        Arc::new(RwLock::new(Environment {
            last_exit_status: 0,
            variable_state,
            policy_state,
            integration_state,
            session_output_state: SessionOutputState {
                output_history: OutputHistory::new(),
                command_blocks: CommandBlockHistory::new(),
            },
            completion_state,
            // A subshell gets a fresh stack: popping in a subshell must not
            // move the parent shell.
            dir_stack: Vec::new(),
            // Likewise a fresh scheduler: only the interactive session runs a
            // task runner, so a subshell's tasks would never fire anyway.
            scheduler: crate::scheduler::SchedulerState::shared(),
            startup_mode: false, // Extended environments (subshells) are not in startup mode
        }))
    }
}

impl std::fmt::Debug for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        let execute_allowlist_len = self.policy_state.execute_allowlist.read().len();
        f.debug_struct("Environment")
            .field("alias", &self.variable_state.alias)
            .field("abbreviations", &self.variable_state.abbreviations)
            .field(
                "command_abbreviations",
                &self.variable_state.command_abbreviations,
            )
            .field(
                "command_ledger_mode",
                &self.variable_state.command_ledger_mode,
            )
            .field("direnv_paths", &self.variable_state.direnv_roots)
            .field("paths", &self.variable_state.paths)
            .field("variables_count", &self.variable_state.variables.len())
            .field("exported_vars", &self.variable_state.exported_vars)
            .field("mcp_servers", &self.integration_state.mcp_servers)
            .field("execute_allowlist_len", &execute_allowlist_len)
            .field(
                "input_preferences",
                &self.completion_state.input_preferences,
            )
            .finish()
    }
}

/// Get the path to a configuration file.
/// User config directories that may hold overrides for an embedded asset
/// directory (`completions`, `output-schemas`), most specific first.
///
/// Both the platform config dir and `~/.config/<app>` are returned: on macOS
/// `dirs::config_dir()` is `~/Library/Application Support`, but users
/// following the docs put overrides in `~/.config/dsh/`, and both must work.
pub fn user_asset_override_dirs(subdir: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(config_dir) = dirs::config_dir() {
        dirs.push(config_dir.join(APP_NAME).join(subdir));
    }

    if let Some(home_dir) = dirs::home_dir() {
        let home_config_dir = home_dir.join(".config").join(APP_NAME).join(subdir);
        if !dirs.contains(&home_config_dir) {
            dirs.push(home_config_dir);
        }
    }

    dirs
}

pub fn get_config_file(name: &str) -> Result<PathBuf> {
    let xdg_dir = xdg::BaseDirectories::with_prefix(APP_NAME);
    xdg_dir.place_config_file(name).context("failed get path")
}

/// Get the path to a data file.
pub fn get_data_file(name: &str) -> Result<PathBuf> {
    #[cfg(test)]
    ensure_test_data_dir();
    let xdg_dir = xdg::BaseDirectories::with_prefix(APP_NAME);
    xdg_dir.place_data_file(name).context("failed get path")
}

/// Get the path to a state file (e.g. logs).
pub fn get_state_file(name: &str) -> Result<PathBuf> {
    #[cfg(test)]
    ensure_test_data_dir();
    let xdg_dir = xdg::BaseDirectories::with_prefix(APP_NAME);
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PathCacheSignature {
    path: String,
    exists: bool,
    modified_secs: Option<u64>,
    len: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExecutableNamesCache {
    version: u8,
    paths: Vec<PathCacheSignature>,
    names: Vec<String>,
}

fn executable_cache_signature(paths: &[String]) -> Vec<PathCacheSignature> {
    paths
        .iter()
        .map(|path| {
            let metadata = std::fs::metadata(path);
            match metadata {
                Ok(metadata) => PathCacheSignature {
                    path: path.clone(),
                    exists: true,
                    modified_secs: metadata
                        .modified()
                        .ok()
                        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                        .map(|duration| duration.as_secs()),
                    len: metadata.len(),
                },
                Err(_) => PathCacheSignature {
                    path: path.clone(),
                    exists: false,
                    modified_secs: None,
                    len: 0,
                },
            }
        })
        .collect()
}

pub fn executable_cache_path() -> Result<PathBuf> {
    get_data_file(EXECUTABLE_CACHE_FILE)
}

pub fn load_cached_executables(paths: &[String]) -> Option<Vec<String>> {
    let path = executable_cache_path().ok()?;
    let contents = std::fs::read_to_string(path).ok()?;
    let cache: ExecutableNamesCache = serde_json::from_str(&contents).ok()?;
    if cache.version == 1 && cache.paths == executable_cache_signature(paths) {
        Some(cache.names)
    } else {
        None
    }
}

pub fn save_cached_executables(paths: &[String], names: &[String]) -> Result<()> {
    let cache = ExecutableNamesCache {
        version: 1,
        paths: executable_cache_signature(paths),
        names: names.to_vec(),
    };
    let path = executable_cache_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string(&cache)?;
    std::fs::write(path, contents)?;
    Ok(())
}
