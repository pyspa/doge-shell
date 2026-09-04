//! Variable and alias resolution.

use super::Environment;
use dsh_types::output_history;
use std::collections::HashMap;

/// Strip the sigil and any braces so `$FOO`, `${FOO}` and `FOO` all reach the
/// same lookup.
fn variable_name(key: &str) -> &str {
    let name = key.strip_prefix('$').unwrap_or(key);
    name.strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .unwrap_or(name)
}

impl Environment {
    /// Get the value of a variable, given `$FOO`, `${FOO}` or a bare `FOO`.
    pub fn get_var(&self, key: &str) -> Option<String> {
        self.lookup_variable(variable_name(key))
    }

    /// Resolve a bare variable name.
    ///
    /// The shell variable map is written to with and without a `$` prefix
    /// depending on which builtin did the writing (`set`/`export` store the
    /// bare name, `read` and Lisp `let` store `$name`), so both spellings are
    /// tried here. Without this, `export FOO=x; echo $FOO` printed nothing
    /// while the child process saw `FOO=x`.
    pub fn lookup_variable(&self, name: &str) -> Option<String> {
        // Shell specials, before anything user-settable can shadow them.
        match name {
            "?" => return Some(self.last_exit_status.to_string()),
            "$" => return Some(std::process::id().to_string()),
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
            _ => {}
        }

        if let Some(value) = self.variable_state.variables.get(name) {
            return Some(value.clone());
        }
        if let Some(value) = self.variable_state.variables.get(&format!("${name}")) {
            return Some(value.clone());
        }

        self.variable_state.system_env_vars.get(name).cloned()
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

    /// Set a process-visible environment variable in the shell snapshot.
    pub fn set_system_env_var(&mut self, key: String, value: String) {
        self.variable_state
            .system_env_vars
            .insert(key.clone(), value);

        self.refresh_derived_state(&key);
    }

    /// Remove a process-visible environment variable from the shell snapshot.
    pub fn unset_system_env_var(&mut self, key: &str) {
        self.variable_state.system_env_vars.remove(key);
        self.refresh_derived_state(key);
    }

    /// Set a shell variable, keeping anything derived from its value in step.
    ///
    /// `PATH` is the reason this exists: writing straight into the map left the
    /// shell looking commands up in the old list while the children it spawned
    /// saw the new one, so `export PATH=...:$PATH; mytool` reported
    /// `command not found` for a tool that was right there.
    pub fn set_shell_var(&mut self, key: String, value: String) {
        self.variable_state.variables.insert(key.clone(), value);
        self.refresh_derived_state(&key);
    }

    /// Mark a shell variable as exported. Exporting changes which value is the
    /// effective one, so the derived state has to be rebuilt as well.
    pub fn export_shell_var(&mut self, key: String) {
        self.variable_state.exported_vars.insert(key.clone());
        self.refresh_derived_state(&key);
    }

    /// Set and export in one step, the way `export NAME=value` does.
    pub fn set_and_export_shell_var(&mut self, key: String, value: String) {
        self.variable_state.variables.insert(key.clone(), value);
        self.variable_state.exported_vars.insert(key.clone());
        self.refresh_derived_state(&key);
    }

    /// Rebuild whatever the shell caches from `key`'s value.
    pub fn refresh_derived_state(&mut self, key: &str) {
        match key {
            "PATH" => self.reload_path(),
            "Z_EXCLUDE" => self.reload_z_exclude(),
            "AI_MESSAGE_LANG" => self.reload_response_language(),
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

    /// The value a child process would be given for `name`.
    ///
    /// An exported shell variable shadows the snapshot the shell started from —
    /// that is what `child_process_env` builds — so anything that reacts to a
    /// variable's *value* has to resolve it the same way, or the shell disagrees
    /// with the processes it launches.
    pub fn effective_env_var(&self, name: &str) -> Option<&str> {
        if self.variable_state.exported_vars.contains(name)
            && let Some(value) = self.variable_state.variables.get(name)
        {
            return Some(value.as_str());
        }
        self.variable_state
            .system_env_vars
            .get(name)
            .map(String::as_str)
    }

    /// Build the effective environment for child processes.
    pub fn child_process_env(&self) -> HashMap<String, String> {
        let mut env_map = self.variable_state.system_env_vars.clone();

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
}
