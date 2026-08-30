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

        match key.as_str() {
            "PATH" => self.reload_path(),
            "Z_EXCLUDE" => self.reload_z_exclude(),
            _ => {}
        }
    }

    /// Remove a process-visible environment variable from the shell snapshot.
    pub fn unset_system_env_var(&mut self, key: &str) {
        self.variable_state.system_env_vars.remove(key);

        match key {
            "PATH" => self.reload_path(),
            "Z_EXCLUDE" => self.reload_z_exclude(),
            _ => {}
        }
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
