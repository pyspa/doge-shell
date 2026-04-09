//! Variable and alias resolution.

use super::Environment;
use dsh_types::output_history;
use std::collections::HashMap;

impl Environment {
    /// Get the value of a variable.
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

    /// Resolves an alias from the Environment's alias map.
    /// If the name is an alias, returns the expanded command; otherwise, returns the original name.
    pub fn resolve_alias(&self, name: &str) -> String {
        self.alias
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// Set a process-visible environment variable in the shell snapshot.
    pub fn set_system_env_var(&mut self, key: String, value: String) {
        self.system_env_vars.insert(key.clone(), value);

        match key.as_str() {
            "PATH" => self.reload_path(),
            "Z_EXCLUDE" => self.reload_z_exclude(),
            _ => {}
        }
    }

    /// Remove a process-visible environment variable from the shell snapshot.
    pub fn unset_system_env_var(&mut self, key: &str) {
        self.system_env_vars.remove(key);

        match key {
            "PATH" => self.reload_path(),
            "Z_EXCLUDE" => self.reload_z_exclude(),
            _ => {}
        }
    }

    /// Build the effective environment for child processes.
    pub fn child_process_env(&self) -> HashMap<String, String> {
        let mut env_map = self.system_env_vars.clone();

        for key in &self.exported_vars {
            if let Some(value) = self.variables.get(key) {
                env_map.insert(key.clone(), value.clone());
            }
        }

        if env_map.get("TERM").is_none_or(|value| value.is_empty()) {
            env_map.insert("TERM".to_string(), "xterm-256color".to_string());
        }

        env_map
    }
}
