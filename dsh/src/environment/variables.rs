//! Variable and alias resolution.

use super::Environment;
use dsh_types::output_history;
use std::collections::HashMap;
use std::sync::Arc;

/// Strip the sigil and any braces so `$FOO`, `${FOO}` and `FOO` all reach the
/// same canonical storage key.
///
/// This is the single canonical-name operation for shell variable storage:
/// `variable_state.variables` and `exported_vars` hold bare names only.
/// `$FOO` / `${FOO}` remain valid parser / parameter-expansion / API input
/// syntax, but they are never storage keys.
pub(crate) fn canonical_shell_var_name(key: &str) -> &str {
    let name = key.strip_prefix('$').unwrap_or(key);
    if name.is_empty() {
        // `$` alone (and `$$` collapsing to `$`) names the PID special.
        // An empty input stays empty so it never aliases the PID entry.
        return if key.is_empty() { "" } else { "$" };
    }
    name.strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .unwrap_or(name)
}

/// Whether `name` is a valid simple shell-variable assignment target.
///
/// Shared by:
/// - `read NAME`
/// - `wait -p NAME`
///
/// Minimum contract shared with parser assignment names:
/// `[A-Za-z_][A-Za-z0-9_]*`. Legacy `set`/`export` keep their own
/// compatibility behavior; this is enforced only on the `read`/`wait -p`
/// paths.
pub(crate) fn is_valid_shell_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Logical shell variable state for reversible overlays.
///
/// One logical variable has exactly one value (`variables`) plus an export
/// attribute (`exported_vars`). Snapshotting both is what lets a direnv
/// overlay restore the pre-activation state exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellVarState {
    pub value: Option<String>,
    pub exported: bool,
}

impl Environment {
    /// Get the value of a variable, given `$FOO`, `${FOO}` or a bare `FOO`.
    pub fn get_var(&self, key: &str) -> Option<String> {
        self.lookup_variable(canonical_shell_var_name(key))
    }

    /// Resolve a bare variable name.
    ///
    /// Storage holds bare names only; `$FOO` / `${FOO}` spellings are
    /// canonicalized on entry so a single map lookup suffices.
    pub fn lookup_variable(&self, name: &str) -> Option<String> {
        let name = canonical_shell_var_name(name);
        // Shell specials, before anything user-settable can shadow them.
        match name {
            "?" => return Some(self.last_exit_status.to_string()),
            "$" => return Some(std::process::id().to_string()),
            // `$!`: PID of the most recently launched async job, empty
            // before the first one. Like `?`/`$`, resolved before any
            // user-settable name so `!` can never be shadowed.
            "!" => {
                return Some(
                    self.last_async_pid
                        .map(|pid| pid.to_string())
                        .unwrap_or_default(),
                );
            }
            _ => {}
        }

        // Captured output: `$OUT`, `$OUT[2]`, `$ERR`, `$ERR[2]`.
        if let Some(index) = output_history::parse_output_var(name, "OUT") {
            return self
                .session_output_state
                .output_history
                .get_stdout(index)
                .map(|s| s.to_string());
        }
        if let Some(index) = output_history::parse_output_var(name, "ERR") {
            return self
                .session_output_state
                .output_history
                .get_stderr(index)
                .map(|s| s.to_string());
        }

        // MCP counters.
        let mcp = |count: usize| Some(count.to_string());
        match name {
            "MCP_SERVERS" => return mcp(self.integration_state.mcp_manager.read().server_count()),
            "MCP_CONNECTED" => {
                return mcp(self.integration_state.mcp_manager.read().connected_count());
            }
            "MCP_TOOLS" => return mcp(self.integration_state.mcp_manager.read().tool_count()),
            "MCP_ACTIVE_TOOLS" => {
                return mcp(self
                    .integration_state
                    .mcp_manager
                    .read()
                    .active_tool_count());
            }
            "MCP_ACTIVE_GROUPS" => {
                let manager = self.integration_state.mcp_manager.read();
                return mcp(manager
                    .tool_groups()
                    .iter()
                    .filter(|group| group.enabled && !manager.is_disabled(&group.name))
                    .count());
            }
            _ => {}
        }

        if let Some(value) = self.variable_state.variables.get(name) {
            return Some(value.clone());
        }

        None
    }

    /// Resolves an alias from the Environment's alias map.
    /// If the name is an alias, returns the expanded command; otherwise, returns the original name.
    pub fn resolve_alias(&self, name: &str) -> String {
        self.variable_state
            .alias
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// Set a shell variable, keeping anything derived from its value in step.
    ///
    /// `PATH` is the reason this exists: writing straight into the map left the
    /// shell looking commands up in the old list while the children it spawned
    /// saw the new one, so `export PATH=...:$PATH; mytool` reported
    /// `command not found` for a tool that was right there.
    ///
    /// Keys are canonicalized to bare names: `$FOO` / `${FOO}` spellings
    /// never reach storage.
    pub fn set_shell_var(&mut self, key: String, value: String) {
        let canonical = canonical_shell_var_name(&key).to_string();
        self.variable_state
            .variables
            .insert(canonical.clone(), value);
        self.refresh_derived_state(&canonical);
    }

    /// Mark a shell variable as exported. Exporting changes which value is the
    /// effective one, so the derived state has to be rebuilt as well.
    pub fn export_shell_var(&mut self, key: String) {
        let canonical = canonical_shell_var_name(&key).to_string();
        self.variable_state.exported_vars.insert(canonical.clone());
        self.refresh_derived_state(&canonical);
    }

    /// Set and export in one step, the way `export NAME=value` does.
    pub fn set_and_export_shell_var(&mut self, key: String, value: String) {
        let canonical = canonical_shell_var_name(&key).to_string();
        self.variable_state
            .variables
            .insert(canonical.clone(), value);
        self.variable_state.exported_vars.insert(canonical.clone());
        self.refresh_derived_state(&canonical);
    }

    /// Remove a shell variable, keeping derived state in step.
    ///
    /// Used to restore lexical scope (Lisp `value-let`): a name that had no
    /// value before the block returns to absent afterwards. The export
    /// attribute is preserved: this only removes the value.
    pub(crate) fn remove_shell_var(&mut self, key: &str) -> Option<String> {
        let canonical = canonical_shell_var_name(key);
        let previous = self.variable_state.variables.remove(canonical);
        self.refresh_derived_state(canonical);
        previous
    }

    /// Logically unset a variable: remove both its value and its export bit.
    ///
    /// Unlike [`Self::remove_shell_var`] (lexical restoration, which keeps
    /// the export attribute), this is the `unset` semantic: afterwards the
    /// name is absent from the shell and from child environments.
    pub(crate) fn unset_shell_var(&mut self, key: &str) {
        let canonical = canonical_shell_var_name(key).to_string();
        self.variable_state.variables.remove(&canonical);
        self.variable_state.exported_vars.remove(&canonical);
        self.refresh_derived_state(&canonical);
    }

    /// Snapshot one logical variable: its value plus its export attribute.
    pub(crate) fn shell_var_state(&self, key: &str) -> ShellVarState {
        let canonical = canonical_shell_var_name(key);
        ShellVarState {
            value: self.variable_state.variables.get(canonical).cloned(),
            exported: self.variable_state.exported_vars.contains(canonical),
        }
    }

    /// Restore one logical variable in a single mutation.
    pub(crate) fn restore_shell_var_state(&mut self, key: &str, state: &ShellVarState) {
        let canonical = canonical_shell_var_name(key).to_string();
        match &state.value {
            Some(value) => {
                self.variable_state
                    .variables
                    .insert(canonical.clone(), value.clone());
            }
            None => {
                self.variable_state.variables.remove(&canonical);
            }
        }
        if state.exported {
            self.variable_state.exported_vars.insert(canonical.clone());
        } else {
            self.variable_state.exported_vars.remove(&canonical);
        }
        self.refresh_derived_state(&canonical);
    }

    /// Rebuild whatever the shell caches from `key`'s value.
    pub fn refresh_derived_state(&mut self, key: &str) {
        match canonical_shell_var_name(key) {
            "PATH" => {
                self.clear_command_cache();
                self.reload_path();
            }
            "Z_EXCLUDE" => self.reload_z_exclude(),
            "AI_MESSAGE_LANG" => self.reload_response_language(),
            "AI_CHAT_MODEL" | "OPENAI_MODEL" => self.reload_chat_model(),
            // The API client snapshots these at construction, so a change
            // has to rebuild it - otherwise a rotated key or a switched
            // endpoint only reaches `!` chat (which resolves its config per
            // message) and never the palette, ghost text, or `ask_ai_async`.
            "AI_CHAT_API_KEY"
            | "OPENAI_API_KEY"
            | "OPEN_AI_API_KEY"
            | "AI_CHAT_BASE_URL"
            | "OPENAI_BASE_URL"
            | "AI_CHAT_TIMEOUT_SECS"
            | "AI_CHAT_REASONING_EFFORT"
            | "AI_CHAT_ALLOW_INSECURE_HTTP" => self.reload_ai_client(),
            _ => {}
        }
    }

    /// Republish `AI_MESSAGE_LANG` to the AI service.
    ///
    /// The service reads the slot, not the map, so setting the variable has to
    /// push the new value across. Without this the setting reached the `!`
    /// runtime alone and every shell-side AI answer stayed in English.
    pub fn reload_response_language(&mut self) {
        let value = self
            .lookup_variable("AI_MESSAGE_LANG")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let changed = self.integration_state.response_language.read().as_ref() != value.as_ref();
        *self.integration_state.response_language.write() = value;
        if changed {
            crate::ai_features::invalidate_read_only_cache();
        }
    }

    /// Republish `AI_CHAT_MODEL`/`OPENAI_MODEL` to the shell-side AI clients.
    ///
    /// `LiveAiService` and the ghost-text backend read this slot, not the
    /// variable map, so setting the variable has to push the new value across.
    /// Without this, `chat_model`/`vset "AI_CHAT_MODEL"` reached the `!`
    /// runtime alone (which reloads its config on every message) and every
    /// other AI path - command palette actions, ghost text, `ai-watch` -
    /// kept using the model resolved at startup.
    ///
    /// `None` means "leave it to the client's own default"
    /// (`dsh_openai::DEFAULT_MODEL`), the same meaning `OpenAiConfig` gives an
    /// absent/blank value.
    pub fn reload_chat_model(&mut self) {
        let value = self
            .lookup_variable("AI_CHAT_MODEL")
            .or_else(|| self.lookup_variable("OPENAI_MODEL"))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let changed = self.integration_state.chat_model.read().as_ref() != value.as_ref();
        *self.integration_state.chat_model.write() = value;
        if changed {
            crate::ai_features::invalidate_read_only_cache();
        }
    }

    /// Rebuild the shared shell-side API client from the current variables.
    ///
    /// `ChatGptClient` snapshots its key, endpoint, and timeouts at
    /// construction. The `!` runtime rebuilds its own client per message and
    /// never needed this, but the shell-side holders (`LiveAiService` via
    /// `SharedChatClient`, the ghost-text backend) share one slot precisely
    /// so this swap reaches all of them at once. A rebuild drops the old
    /// client's 400-recovery learning (it is per-client memory); the new
    /// client re-learns within one retry.
    ///
    /// `None` (no key) clears the slot; every shell-side caller treats that
    /// as "not configured", the same as a missing `ai_service` used to.
    ///
    /// Reads only the shell variable map: `Environment::new()` already
    /// imported the startup process environment, so a process-global
    /// fallback here would resurrect a key the shell explicitly unset.
    pub fn reload_ai_client(&mut self) {
        let config = dsh_openai::OpenAiConfig::from_getter(|key| self.lookup_variable(key));
        let client = match config.api_key() {
            None => None,
            Some(_) => match dsh_openai::ChatGptClient::try_from_config(&config) {
                Ok(client) => Some(Arc::new(client)),
                Err(e) => {
                    tracing::debug!("ai client reload failed, treating as not configured: {e}");
                    None
                }
            },
        };
        *self.integration_state.ai_client.write() = client;
    }

    /// Rebuild every variable-derived projection at once.
    ///
    /// For bulk raw-map restore/apply (config rollback, child snapshot
    /// apply): single-variable mutation keeps using `refresh_derived_state`.
    pub(crate) fn refresh_variable_projections(&mut self) {
        self.reload_path();
        self.reload_z_exclude();
        self.reload_response_language();
        self.reload_chat_model();
        self.reload_ai_client();
    }

    /// Whether a shell-side AI request can currently be sent.
    ///
    /// True when the shared client slot holds a client (i.e. an API key is
    /// configured). Shell-side holders (`LiveAiService`, the ghost-text
    /// backend) are now always constructed and follow the slot, so callers
    /// must ask this instead of checking `ai_service.is_some()`.
    pub fn ai_configured(&self) -> bool {
        self.integration_state.ai_client.read().is_some()
    }

    /// The shell-side AI service when one can currently be used.
    ///
    /// One call instead of an `ai_configured` check plus a clone at every
    /// call site. Returns the stored service only while the shared client
    /// slot holds a client.
    pub fn live_ai_service(&self) -> Option<Arc<dyn crate::ai_features::AiService + Send + Sync>> {
        if !self.ai_configured() {
            return None;
        }
        self.integration_state.ai_service.clone()
    }

    /// Build the effective environment for child processes.
    ///
    /// The single materializer: every exported shell variable's current
    /// value, nothing else. `Process::prepare_execution` shares the same
    /// precedence (command-scoped overrides win, then this map, then the
    /// `TERM` fallback).
    pub fn child_process_env(&self) -> HashMap<String, String> {
        let mut env_map = HashMap::with_capacity(self.variable_state.exported_vars.len() + 1);

        for key in &self.variable_state.exported_vars {
            if let Some(value) = self.variable_state.variables.get(key) {
                env_map.insert(key.clone(), value.clone());
            }
        }

        if env_map.get("TERM").is_none_or(|value| value.is_empty()) {
            env_map.insert("TERM".to_string(), "xterm-256color".to_string());
        }

        env_map
    }

    /// Iterate exported variables without allocating a map.
    ///
    /// Shared precedence helper for `Process::prepare_execution`: the same
    /// `variables + exported_vars` view as [`Self::child_process_env`], but
    /// pushed through a callback so per-spawn allocation stays minimal.
    pub(crate) fn for_each_exported_var(&self, mut f: impl FnMut(&str, &str)) {
        for key in &self.variable_state.exported_vars {
            if let Some(value) = self.variable_state.variables.get(key) {
                f(key.as_str(), value.as_str());
            }
        }
    }
}
