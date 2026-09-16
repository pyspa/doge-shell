mod connection;
mod exec;
mod naming;
mod servers;
use anyhow::Result;
use dsh_types::mcp::{McpServerConfig, McpTransport};
use naming::*;
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, Tool},
    transport::{
        child_process::TokioChildProcess,
        streamable_http_client::{
            StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
        },
    },
};
use serde_json::{Value, json};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::OpenOptions;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::{
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use tokio::{process::Command, time::timeout};
use tracing::{debug, info, warn};
use xdg::BaseDirectories;

/// Default timeout for MCP tool calls (30 seconds)
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const LEGACY_SSE_UNSUPPORTED_MESSAGE: &str = "Legacy SSE MCP transport is configuration-only; rmcp 1.7 removed the legacy SSE client transport. Use streamable HTTP via mcp-add-http instead.";

#[derive(Debug)]
pub struct McpCallError {
    message: String,
    outcome_unknown: bool,
}

impl McpCallError {
    fn failure(message: impl ToString) -> Self {
        Self {
            message: message.to_string(),
            outcome_unknown: false,
        }
    }

    fn from_request(error: connection::McpRequestError, context: &str) -> Self {
        let outcome_unknown = error.outcome_unknown();
        let message = if outcome_unknown {
            format!("{context}; outcome unknown: {error}")
        } else {
            format!("{context}: {error}")
        };
        Self {
            outcome_unknown,
            message,
        }
    }

    pub fn outcome_unknown(&self) -> bool {
        self.outcome_unknown
    }
}

impl std::fmt::Display for McpCallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for McpCallError {}

/// Status of a MCP server connection
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpConnectionStatus {
    /// Server is registered but not connected
    Disconnected,
    /// Server is connected and ready
    Connected,
    /// Server connection failed
    Error(String),
}

impl std::fmt::Display for McpConnectionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected => write!(f, "disconnected"),
            Self::Connected => write!(f, "connected"),
            Self::Error(msg) => write!(f, "error: {msg}"),
        }
    }
}

/// Information about a MCP server's status
#[derive(Debug, Clone)]
pub struct McpServerStatus {
    pub label: String,
    pub description: Option<String>,
    pub transport_type: String,
    pub status: McpConnectionStatus,
    pub tool_count: usize,
    pub connected_since: Option<Instant>,
}

#[derive(Debug, Clone)]
struct McpServer {
    label: String,
    description: Option<String>,
    transport: McpTransport,
    tools: Vec<Tool>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ToolCacheEntry {
    config_hash: u64,
    timestamp: i64,
    tools: Vec<Tool>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct McpToolCache {
    entries: HashMap<String, ToolCacheEntry>,
}

impl McpToolCache {
    fn load() -> Self {
        let dirs = BaseDirectories::with_prefix("dogesh");
        if let Some(path) = dirs.find_cache_file("mcp_tools.json")
            && let Ok(file) = std::fs::File::open(path)
            && let Ok(cache) = serde_json::from_reader(file)
        {
            return cache;
        }
        Self::default()
    }

    fn save(&self) {
        let dirs = BaseDirectories::with_prefix("dogesh");
        if let Ok(path) = dirs.place_cache_file("mcp_tools.json")
            && let Ok(file) = std::fs::File::create(path)
        {
            let _ = serde_json::to_writer(file, self);
        }
    }
}

// Helper to hash McpServerConfig manually since it doesn't derive Hash
fn hash_server_config(config: &McpServerConfig) -> u64 {
    let mut s = DefaultHasher::new();
    config.label.hash(&mut s);
    config.description.hash(&mut s);
    // Transport
    match &config.transport {
        McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            0u8.hash(&mut s);
            command.hash(&mut s);
            args.hash(&mut s);
            // Sort env for consistent hash
            let sorted_env: BTreeMap<_, _> = env.iter().collect();
            sorted_env.hash(&mut s);
            cwd.hash(&mut s);
        }
        McpTransport::Sse { url } => {
            1u8.hash(&mut s);
            url.hash(&mut s);
        }
        McpTransport::Http {
            url,
            auth_header,
            allow_stateless,
        } => {
            2u8.hash(&mut s);
            url.hash(&mut s);
            auth_header.hash(&mut s);
            allow_stateless.hash(&mut s);
        }
    }
    s.finish()
}

#[derive(Debug, Clone)]
struct ToolBinding {
    server_label: String,
    tool_name: String,
    function_name: String,
}

/// Cached session metadata (session ownership is managed separately)
struct SessionMeta {
    connected_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct McpSyncStats {
    pub removed: usize,
    pub updated: usize,
    pub added: usize,
    pub unchanged: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpRuntimeStateSnapshot {
    pub session_meta: HashMap<String, Instant>,
    pub connection_errors: HashMap<String, String>,
}

/// MCP Manager with session caching support
pub struct McpManager {
    connections: parking_lot::Mutex<HashMap<String, Arc<connection::Connection>>>,
    tools_refreshed: Instant,
    servers: Vec<McpServer>,
    /// A `BTreeMap`, not a `HashMap`: `tool_definitions` walks it to build the
    /// `tools` array, and a hash order that changes between processes means the
    /// provider's prefix cache misses on every fresh shell.
    bindings: BTreeMap<String, ToolBinding>,
    warnings: Vec<String>,
    /// Metadata for connected sessions
    session_meta: RwLock<HashMap<String, SessionMeta>>,
    /// Connection errors for status reporting
    connection_errors: RwLock<HashMap<String, String>>,
    /// Servers the user disconnected with `mcp disconnect`.
    ///
    /// Kept apart from `session_meta`, which only ever holds servers an
    /// explicit `mcp connect` validated: gating tool calls on that would hide
    /// every server the shell loaded at startup. `disconnect` used to clear
    /// metadata alone, so `mcp status` said "disconnected" while the agent
    /// carried on calling the tools.
    disabled: RwLock<HashSet<String>>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            bindings: BTreeMap::new(),
            warnings: Vec::new(),
            session_meta: RwLock::new(HashMap::new()),
            connection_errors: RwLock::new(HashMap::new()),
            disabled: RwLock::new(HashSet::new()),
            connections: parking_lot::Mutex::new(HashMap::new()),
            tools_refreshed: Instant::now(),
        }
    }
}

impl std::fmt::Debug for McpManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpManager")
            .field("servers", &self.servers.len())
            .field("bindings", &self.bindings.len())
            .field("warnings", &self.warnings)
            .finish()
    }
}

impl McpManager {
    fn session_meta_read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, SessionMeta>> {
        self.session_meta.read().unwrap_or_else(|poisoned| {
            warn!("session metadata lock poisoned; recovering read access");
            poisoned.into_inner()
        })
    }

    fn session_meta_write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, SessionMeta>> {
        self.session_meta.write().unwrap_or_else(|poisoned| {
            warn!("session metadata lock poisoned; recovering write access");
            poisoned.into_inner()
        })
    }

    fn disabled_read(&self) -> std::sync::RwLockReadGuard<'_, HashSet<String>> {
        self.disabled.read().unwrap_or_else(|poisoned| {
            warn!("disabled server lock poisoned; recovering read access");
            poisoned.into_inner()
        })
    }

    fn disabled_write(&self) -> std::sync::RwLockWriteGuard<'_, HashSet<String>> {
        self.disabled.write().unwrap_or_else(|poisoned| {
            warn!("disabled server lock poisoned; recovering write access");
            poisoned.into_inner()
        })
    }

    /// Whether `mcp disconnect` has taken this server out of service.
    pub fn is_disabled(&self, label: &str) -> bool {
        self.disabled_read().contains(label)
    }

    fn connection_errors_read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, String>> {
        self.connection_errors.read().unwrap_or_else(|poisoned| {
            warn!("connection errors lock poisoned; recovering read access");
            poisoned.into_inner()
        })
    }

    fn connection_errors_write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, String>> {
        self.connection_errors.write().unwrap_or_else(|poisoned| {
            warn!("connection errors lock poisoned; recovering write access");
            poisoned.into_inner()
        })
    }

    pub async fn load(runtime_servers: Vec<McpServerConfig>) -> Self {
        match Self::build_from_servers(runtime_servers).await {
            Ok(manager) => manager,
            Err(err) => {
                warn!("failed to initialize MCP manager: {err:?}");
                Self::default()
            }
        }
    }

    pub fn load_blocking(runtime_servers: Vec<McpServerConfig>) -> Self {
        match Self::execute_async_with_loader(move || async move {
            Self::build_from_servers(runtime_servers).await
        }) {
            Ok(manager) => manager,
            Err(err) => {
                warn!("failed to initialize MCP manager: {err:?}");
                Self::default()
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Get the number of registered servers
    pub fn server_count(&self) -> usize {
        self.servers.len()
    }

    /// Get the number of connected servers (based on metadata)
    pub fn connected_count(&self) -> usize {
        self.session_meta_read().len()
    }

    /// Get the currently registered MCP server configurations.
    pub fn server_configs(&self) -> Vec<McpServerConfig> {
        self.servers
            .iter()
            .map(|server| McpServerConfig {
                label: server.label.clone(),
                description: server.description.clone(),
                transport: server.transport.clone(),
            })
            .collect()
    }

    /// Get the total number of available tools
    pub fn tool_count(&self) -> usize {
        self.bindings.len()
    }

    /// Get status of all registered MCP servers
    pub fn get_status(&self) -> Vec<McpServerStatus> {
        let meta = self.session_meta_read();
        let errors = self.connection_errors_read();

        self.servers
            .iter()
            .map(|server| {
                let (status, connected_since) = if let Some(m) = meta.get(&server.label) {
                    (McpConnectionStatus::Connected, Some(m.connected_at))
                } else if let Some(err) = errors.get(&server.label) {
                    (McpConnectionStatus::Error(err.clone()), None)
                } else {
                    (McpConnectionStatus::Disconnected, None)
                };

                let transport_type = match &server.transport {
                    McpTransport::Stdio { .. } => "stdio",
                    McpTransport::Sse { .. } => "sse",
                    McpTransport::Http { .. } => "http",
                };

                McpServerStatus {
                    label: server.label.clone(),
                    description: server.description.clone(),
                    transport_type: transport_type.to_string(),
                    status,
                    tool_count: server.tools.len(),
                    connected_since,
                }
            })
            .collect()
    }

    /// Snapshot mutable runtime metadata (connection state and last errors).
    ///
    /// This intentionally excludes static server/tool definitions.
    pub fn snapshot_runtime_state(&self) -> McpRuntimeStateSnapshot {
        let session_meta = self
            .session_meta_read()
            .iter()
            .map(|(label, meta)| (label.clone(), meta.connected_at))
            .collect();
        let connection_errors = self.connection_errors_read().clone();
        McpRuntimeStateSnapshot {
            session_meta,
            connection_errors,
        }
    }

    /// Restore mutable runtime metadata from a snapshot.
    pub fn restore_runtime_state(&self, snapshot: McpRuntimeStateSnapshot) {
        let McpRuntimeStateSnapshot {
            session_meta,
            connection_errors,
        } = snapshot;

        let mut meta_lock = self.session_meta_write();
        meta_lock.clear();
        meta_lock.extend(
            session_meta
                .into_iter()
                .map(|(label, connected_at)| (label, SessionMeta { connected_at })),
        );

        *self.connection_errors_write() = connection_errors;
    }

    /// Connect to a specific MCP server (validates connectivity)
    pub fn connect(&self, label: &str) -> Result<(), String> {
        let server = self
            .servers
            .iter()
            .find(|s| s.label == label)
            .ok_or_else(|| format!("MCP server '{}' not found", label))?;

        // Test connection by listing tools
        let transport = server.transport.clone();
        let label_owned = label.to_string();

        match Self::execute_async_with_loader(
            move || async move { test_connection(transport).await },
        ) {
            Ok(()) => {
                info!(server = label, "validated MCP server connection");
                self.disabled_write().remove(&label_owned);
                self.session_meta_write().insert(
                    label_owned.clone(),
                    SessionMeta {
                        connected_at: Instant::now(),
                    },
                );
                self.connection_errors_write().remove(&label_owned);
                Ok(())
            }
            Err(err) => {
                let error_msg = format!("{err}");
                self.connection_errors_write()
                    .insert(label_owned, error_msg.clone());
                Err(error_msg)
            }
        }
    }

    /// Disconnect from a specific MCP server.
    ///
    /// Its tools leave `tool_definitions`, so the model stops being offered
    /// them, and `execute_tool` refuses a call the model made before the
    /// disconnect landed. Clearing the session metadata alone left `mcp status`
    /// saying "disconnected" while the agent kept calling the same tools.
    pub fn disconnect(&self, label: &str) -> Result<(), String> {
        self.connections.lock().remove(label);
        if !self.servers.iter().any(|server| server.label == label) {
            return Err(format!("MCP server '{label}' not found"));
        }
        self.session_meta_write().remove(label);
        self.disabled_write().insert(label.to_string());
        info!(server = label, "disconnected from MCP server");
        Ok(())
    }

    /// Disconnect from all MCP servers
    pub fn disconnect_all(&self) {
        self.connections.lock().clear();
        self.session_meta_write().clear();
        let mut disabled = self.disabled_write();
        let mut count = 0usize;
        for server in &self.servers {
            if disabled.insert(server.label.clone()) {
                count += 1;
            }
        }
        if count > 0 {
            info!(count, "disconnected from all MCP servers");
        }
    }

    pub fn refresh_tools_if_expired(&mut self, cancelled: &dyn Fn() -> bool) -> Result<(), String> {
        if cancelled() {
            return Err("MCP discovery cancelled".into());
        }
        if self.tools_refreshed.elapsed() < Duration::from_secs(300)
            && !self
                .connections
                .lock()
                .values()
                .any(|connection| connection.tools_changed())
        {
            return Ok(());
        }
        for index in 0..self.servers.len() {
            if cancelled() {
                return Err("MCP discovery cancelled".into());
            }
            let label = self.servers[index].label.clone();
            if self.is_disabled(&label) {
                continue;
            }
            let transport = self.servers[index].transport.clone();
            let connection = self
                .connections
                .lock()
                .entry(label.clone())
                .or_insert_with(|| Arc::new(connection::Connection::new(transport)))
                .clone();
            let fetched = connection
                .request(json!({"kind":"list"}), cancelled)
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<Vec<Tool>>(value).map_err(|error| error.to_string())
                });
            if cancelled() {
                return Err("MCP discovery cancelled".into());
            }
            let tools = match fetched {
                Ok(tools) => {
                    self.connection_errors_write().remove(&label);
                    tools
                }
                Err(error) => {
                    self.connection_errors_write()
                        .insert(label.clone(), error.to_string());
                    // Keep this server's cached definitions; a failed refresh
                    // must not hide healthy servers from discovery.
                    connection.mark_refreshed();
                    continue;
                }
            };
            self.bindings
                .retain(|_, binding| binding.server_label != label);
            for tool in &tools {
                let base = format!(
                    "mcp__{}__{}",
                    sanitize_identifier(&label),
                    sanitize_identifier(&tool.name)
                );
                let name = stable_name(&base, &label, &tool.name);
                self.bindings.insert(
                    name.clone(),
                    ToolBinding {
                        server_label: label.clone(),
                        tool_name: tool.name.to_string(),
                        function_name: name,
                    },
                );
            }
            self.servers[index].tools = tools;
            connection.mark_refreshed();
        }
        self.tools_refreshed = Instant::now();
        Ok(())
    }

    pub fn tool_definitions(&self) -> Vec<Value> {
        let disabled = self.disabled_read();
        self.bindings
            .values()
            .filter_map(|binding| {
                if disabled.contains(&binding.server_label) {
                    return None;
                }
                let server = self
                    .servers
                    .iter()
                    .find(|srv| srv.label == binding.server_label)?;
                let tool = server
                    .tools
                    .iter()
                    .find(|tool| tool.name.as_ref() == binding.tool_name)?;

                let schema = Value::Object((*tool.input_schema).clone());
                let description = match (&server.description, &tool.description) {
                    (Some(server_desc), Some(tool_desc)) if !server_desc.is_empty() => format!(
                        "MCP server `{}` — {}\nTool `{}`: {}",
                        server.label, server_desc, tool.name, tool_desc
                    ),
                    (Some(server_desc), _) if !server_desc.is_empty() => format!(
                        "MCP server `{}` — {}\nTool `{}`",
                        server.label, server_desc, tool.name
                    ),
                    (_, Some(tool_desc)) if !tool_desc.is_empty() => format!(
                        "MCP server `{}` tool `{}`: {}",
                        server.label, tool.name, tool_desc
                    ),
                    _ => format!("MCP server `{}` tool `{}`", server.label, tool.name),
                };

                let function_name = binding.function_name.clone();

                Some(json!({
                    "type": "function",
                    "function": {
                        "name": function_name,
                        "description": description,
                        "parameters": schema,
                    }
                }))
            })
            .collect()
    }

    pub fn system_prompt_fragment(&self) -> Option<String> {
        let disabled = self.disabled_read();
        if self
            .servers
            .iter()
            .all(|server| disabled.contains(&server.label))
        {
            return None;
        }

        let mut lines = vec![
            "You can call external Model Context Protocol (MCP) servers when solving tasks."
                .to_string(),
            "Always prefer the dedicated MCP function tools when they cover the action you need."
                .to_string(),
            "Note: Tool execution may be rejected by the user for safety reasons. If rejected, propose an alternative approach."
                .to_string(),
            "Be cautious when using tools that modify the filesystem or execute commands."
                .to_string(),
        ];
        if !self.warnings.is_empty() {
            lines.push("Warnings: ".to_string());
            for warning in &self.warnings {
                lines.push(format!("- {warning}"));
            }
        }

        // Only the servers belong here. Every tool already reaches the model
        // through the `tools` array with its own name, description and schema;
        // repeating them in the prompt paid for the same text twice.
        for server in &self.servers {
            if disabled.contains(&server.label) {
                continue;
            }
            let mut header = format!("- Server `{}`", server.label);
            if let Some(desc) = &server.description
                && !desc.trim().is_empty()
            {
                header.push_str(&format!(": {desc}"));
            }
            header.push_str(&format!(" ({} tools)", server.tools.len()));
            lines.push(header);
        }

        Some(lines.join("\n"))
    }
}
#[cfg(test)]
mod tests;
