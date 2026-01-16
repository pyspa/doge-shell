//! MCP server management.

use super::Environment;
use dsh_types::mcp::McpServerConfig;

impl Environment {
    /// Clear all MCP server configurations.
    pub fn clear_mcp_servers(&mut self) {
        self.mcp_servers.clear();
    }

    /// Add an MCP server configuration.
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

    /// Get all MCP server configurations.
    pub fn mcp_servers(&self) -> &[McpServerConfig] {
        &self.mcp_servers
    }

    /// Clear the execute allowlist.
    pub fn clear_execute_allowlist(&self) {
        self.execute_allowlist.write().clear();
    }

    /// Add an entry to the execute allowlist.
    pub fn add_execute_allowlist_entry(&self, entry: String) {
        let mut allowlist = self.execute_allowlist.write();
        if !allowlist.contains(&entry) {
            allowlist.push(entry);
        }
    }

    /// Get the execute allowlist.
    pub fn execute_allowlist(&self) -> Vec<String> {
        self.execute_allowlist.read().clone()
    }
}
