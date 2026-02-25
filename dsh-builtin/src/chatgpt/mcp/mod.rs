use anyhow::Result;
use dsh_types::mcp::{McpServerConfig, McpTransport};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, CallToolResult, ListToolsResult, Tool},
    transport::{
        child_process::TokioChildProcess,
        streamable_http_client::{
            StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
        },
    },
};
use serde_json::{Map, Value, json};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::{
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use tokio::{process::Command, time::timeout};
use tracing::{debug, info, warn};
use xdg::BaseDirectories;

/// Default timeout for MCP tool calls (30 seconds)
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(30);

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
        if let Ok(dirs) = BaseDirectories::with_prefix("dsh")
            && let Some(path) = dirs.find_cache_file("mcp_tools.json")
            && let Ok(file) = std::fs::File::open(path)
            && let Ok(cache) = serde_json::from_reader(file)
        {
            return cache;
        }
        Self::default()
    }

    fn save(&self) {
        if let Ok(dirs) = BaseDirectories::with_prefix("dsh")
            && let Ok(path) = dirs.place_cache_file("mcp_tools.json")
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
    servers: Vec<McpServer>,
    bindings: HashMap<String, ToolBinding>,
    warnings: Vec<String>,
    /// Metadata for connected sessions
    session_meta: RwLock<HashMap<String, SessionMeta>>,
    /// Connection errors for status reporting
    connection_errors: RwLock<HashMap<String, String>>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            bindings: HashMap::new(),
            warnings: Vec::new(),
            session_meta: RwLock::new(HashMap::new()),
            connection_errors: RwLock::new(HashMap::new()),
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
        self.session_meta.read().unwrap().len()
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
        let meta = self.session_meta.read().unwrap();
        let errors = self.connection_errors.read().unwrap();

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
            .session_meta
            .read()
            .unwrap()
            .iter()
            .map(|(label, meta)| (label.clone(), meta.connected_at))
            .collect();
        let connection_errors = self.connection_errors.read().unwrap().clone();
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

        let mut meta_lock = self.session_meta.write().unwrap();
        meta_lock.clear();
        meta_lock.extend(
            session_meta
                .into_iter()
                .map(|(label, connected_at)| (label, SessionMeta { connected_at })),
        );

        *self.connection_errors.write().unwrap() = connection_errors;
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
                self.session_meta.write().unwrap().insert(
                    label_owned.clone(),
                    SessionMeta {
                        connected_at: Instant::now(),
                    },
                );
                self.connection_errors.write().unwrap().remove(&label_owned);
                Ok(())
            }
            Err(err) => {
                let error_msg = format!("{err}");
                self.connection_errors
                    .write()
                    .unwrap()
                    .insert(label_owned, error_msg.clone());
                Err(error_msg)
            }
        }
    }

    /// Disconnect from a specific MCP server (clears metadata)
    pub fn disconnect(&self, label: &str) -> Result<(), String> {
        self.session_meta.write().unwrap().remove(label);
        info!(server = label, "disconnected from MCP server");
        Ok(())
    }

    /// Disconnect from all MCP servers
    pub fn disconnect_all(&self) {
        let count = self.session_meta.write().unwrap().drain().count();
        if count > 0 {
            info!(count, "disconnected from all MCP servers");
        }
    }

    pub fn tool_definitions(&self) -> Vec<Value> {
        self.bindings
            .values()
            .filter_map(|binding| {
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
        if self.servers.is_empty() {
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

        for server in &self.servers {
            let mut header = format!("- Server `{}`", server.label);
            if let Some(desc) = &server.description
                && !desc.trim().is_empty()
            {
                header.push_str(&format!(": {desc}"));
            }
            lines.push(header);
            for tool in &server.tools {
                if let Some(tool_desc) = &tool.description {
                    lines.push(format!("  • Tool `{}` – {}", tool.name, tool_desc));
                } else {
                    lines.push(format!("  • Tool `{}`", tool.name));
                }
            }
        }

        Some(lines.join("\n"))
    }

    pub fn execute_tool(
        &self,
        function_name: &str,
        arguments: &str,
    ) -> Result<Option<String>, String> {
        let binding = match self.bindings.get(function_name) {
            Some(binding) => binding,
            None => return Ok(None),
        };

        let args_value: Value = if arguments.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(arguments)
                .map_err(|err| format!("failed to parse MCP tool arguments: {err}"))?
        };

        let map = match args_value {
            Value::Null => None,
            Value::Object(map) => Some(map),
            other => {
                return Err(format!(
                    "expected MCP tool arguments to be an object, got {other}"
                ));
            }
        };

        let transport = self
            .servers
            .iter()
            .find(|srv| srv.label == binding.server_label)
            .map(|srv| srv.transport.clone())
            .ok_or_else(|| "MCP server missing for tool invocation".to_string())?;
        let tool_name = binding.tool_name.clone();

        let result = Self::execute_async_with_loader(move || async move {
            timeout(
                DEFAULT_TOOL_TIMEOUT,
                call_tool_via_transport(transport, &tool_name, map),
            )
            .await
            .map_err(|_| anyhow::anyhow!("MCP tool call timed out after 30 seconds"))?
        })
        .map_err(|err| format!("failed to call MCP tool: {err}"))?;

        let json = serde_json::to_value(&result)
            .map_err(|err| format!("failed to serialize MCP tool result: {err}"))?;

        Ok(Some(json.to_string()))
    }

    async fn build_from_servers(runtime_servers: Vec<McpServerConfig>) -> Result<Self> {
        let mut servers = Vec::new();
        let mut labels = HashSet::new();
        let mut warnings = Vec::new();

        // Dedup and validate first
        let mut valid_configs = Vec::new();
        for server in runtime_servers {
            if server.label.trim().is_empty() {
                warnings.push("skipped MCP server with empty label".to_string());
                continue;
            }
            if !labels.insert(server.label.clone()) {
                warnings.push(format!(
                    "skipped MCP server `{}` because the label is duplicated",
                    server.label
                ));
                continue;
            }
            valid_configs.push(server);
        }

        let mut cache = McpToolCache::load();
        let mut cache_changed = false;

        // Prepare futures for parallel execution
        let mut futures = Vec::new();

        for config in valid_configs {
            let hash = hash_server_config(&config);
            let cached_tools: Option<Vec<Tool>> =
                if let Some(entry) = cache.entries.get(&config.label) {
                    if entry.config_hash == hash {
                        debug!("Loaded tools for {} from cache", config.label);
                        Some(entry.tools.clone())
                    } else {
                        None
                    }
                } else {
                    None
                };

            futures.push(async move {
                if let Some(tools) = cached_tools {
                    Ok((config, tools, false)) // false = not new
                } else {
                    match list_tools_via_transport(config.transport.clone()).await {
                        Ok(tools) => Ok((config, tools, true)), // true = new/refreshed
                        Err(e) => Err((config.label, e)),
                    }
                }
            });
        }

        // Execute sequentially logic is replaced by parallel logic below
        // Execute sequentially to prevent process storm
        // Execute sequentially to prevent process storm
        let mut results = Vec::new();
        for (i, future) in futures.into_iter().enumerate() {
            // Add a small delay between server loads to yield CPU/IO to the main thread
            if i > 0 {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            results.push(future.await);
        }

        for res in results {
            match res {
                Ok((config, tools, updated)) => {
                    if updated {
                        let hash = hash_server_config(&config);
                        cache.entries.insert(
                            config.label.clone(),
                            ToolCacheEntry {
                                config_hash: hash,
                                timestamp: chrono::Utc::now().timestamp(),
                                tools: tools.clone(),
                            },
                        );
                        cache_changed = true;
                    }

                    debug!(
                        server = config.label.as_str(),
                        tool_count = tools.len(),
                        "registered MCP server"
                    );

                    servers.push(McpServer {
                        label: config.label,
                        description: config.description,
                        transport: config.transport,
                        tools,
                    });
                }
                Err((label, err)) => {
                    warnings.push(format!("failed to load MCP server `{}`: {}", label, err));
                }
            }
        }

        if cache_changed {
            cache.save();
        }

        let mut bindings = HashMap::new();
        let mut used_names = HashSet::new();

        for server in &servers {
            for tool in &server.tools {
                let base_name = format!(
                    "mcp__{}__{}",
                    sanitize_identifier(&server.label),
                    sanitize_identifier(tool.name.as_ref())
                );
                let function_name = unique_name(&base_name, &mut used_names);
                bindings.insert(
                    function_name.clone(),
                    ToolBinding {
                        server_label: server.label.clone(),
                        tool_name: tool.name.to_string(),
                        function_name,
                    },
                );
            }
        }

        Ok(Self {
            servers,
            bindings,
            warnings,
            session_meta: RwLock::new(HashMap::new()),
            connection_errors: RwLock::new(HashMap::new()),
        })
    }

    pub async fn add_server(&mut self, config: McpServerConfig) -> Result<()> {
        let (server_struct, tools) = Self::load_server_tools(config).await?;
        self.register_server(server_struct, tools)?;
        Ok(())
    }

    pub fn add_server_blocking(&mut self, config: McpServerConfig) -> Result<()> {
        let config_clone = config.clone();
        let (server_struct, tools) = Self::execute_async_with_loader(move || async move {
            Self::load_server_tools(config_clone).await
        })?;
        self.register_server(server_struct, tools)?;
        Ok(())
    }

    /// Remove a registered MCP server and its associated metadata.
    pub fn remove_server(&mut self, label: &str) -> bool {
        let before = self.servers.len();
        self.servers.retain(|server| server.label != label);
        let removed = self.servers.len() != before;
        if removed {
            self.bindings
                .retain(|_, binding| binding.server_label != label);
            self.session_meta.write().unwrap().remove(label);
            self.connection_errors.write().unwrap().remove(label);
        }
        removed
    }

    fn replace_server_blocking(&mut self, config: McpServerConfig) -> Result<()> {
        let label = config.label.clone();
        let config_clone = config.clone();
        let (server_struct, tools) = Self::execute_async_with_loader(move || async move {
            Self::load_server_tools(config_clone).await
        })?;
        self.remove_server(&label);
        self.register_server(server_struct, tools)?;
        Ok(())
    }

    /// Synchronize the registered servers to the desired configuration set.
    ///
    /// This applies add/update/remove as a diff by label:
    /// - removed: label exists currently but not in desired
    /// - updated: label exists in both, but config changed
    /// - added: label exists only in desired
    pub fn sync_servers_blocking(&mut self, desired_servers: Vec<McpServerConfig>) -> McpSyncStats {
        let current_by_label: HashMap<String, McpServerConfig> = self
            .server_configs()
            .into_iter()
            .map(|config| (config.label.clone(), config))
            .collect();

        let mut desired_by_label: HashMap<String, McpServerConfig> = HashMap::new();
        for server in desired_servers {
            if server.label.trim().is_empty() {
                warn!("skipped MCP server with empty label during sync");
                continue;
            }
            if desired_by_label.contains_key(&server.label) {
                warn!(
                    "skipped duplicated MCP server label `{}` during sync",
                    server.label
                );
                continue;
            }
            desired_by_label.insert(server.label.clone(), server);
        }

        let mut stats = McpSyncStats::default();

        let mut labels_to_remove: Vec<String> = current_by_label
            .keys()
            .filter(|label| !desired_by_label.contains_key(*label))
            .cloned()
            .collect();
        labels_to_remove.sort_unstable();
        for label in labels_to_remove {
            if self.remove_server(&label) {
                stats.removed += 1;
            }
        }

        let mut desired_labels: Vec<String> = desired_by_label.keys().cloned().collect();
        desired_labels.sort_unstable();
        for label in desired_labels {
            let desired = match desired_by_label.remove(&label) {
                Some(server) => server,
                None => continue,
            };
            match current_by_label.get(&label) {
                Some(current) if current == &desired => {
                    stats.unchanged += 1;
                }
                Some(_) => match self.replace_server_blocking(desired) {
                    Ok(_) => stats.updated += 1,
                    Err(err) => {
                        warn!(server = label, "failed to update MCP server: {err}");
                    }
                },
                None => match self.add_server_blocking(desired) {
                    Ok(_) => stats.added += 1,
                    Err(err) => {
                        warn!(server = label, "failed to add MCP server: {err}");
                    }
                },
            }
        }

        stats
    }

    async fn load_server_tools(config: McpServerConfig) -> Result<(McpServer, Vec<Tool>)> {
        let McpServerConfig {
            label,
            description,
            transport,
        } = config;

        let tools = list_tools_via_transport(transport.clone())
            .await
            .map_err(|err| anyhow::anyhow!("failed to load MCP server `{}`: {}", label, err))?;

        debug!(
            server = label.as_str(),
            tool_count = tools.len(),
            "registered MCP server"
        );

        let server_struct = McpServer {
            label: label.clone(),
            description,
            transport,
            tools: tools.clone(),
        };

        Ok((server_struct, tools))
    }

    fn register_server(&mut self, server: McpServer, tools: Vec<Tool>) -> Result<()> {
        if self.servers.iter().any(|s| s.label == server.label) {
            return Err(anyhow::anyhow!(
                "MCP server `{}` already exists",
                server.label
            ));
        }

        // Update bindings
        let mut used_names: HashSet<String> = self.bindings.keys().cloned().collect();

        for tool in &tools {
            let base_name = format!(
                "mcp__{}__{}",
                sanitize_identifier(&server.label),
                sanitize_identifier(tool.name.as_ref())
            );
            let function_name = unique_name(&base_name, &mut used_names);
            self.bindings.insert(
                function_name.clone(),
                ToolBinding {
                    server_label: server.label.clone(),
                    tool_name: tool.name.to_string(),
                    function_name,
                },
            );
        }

        self.servers.push(server);
        Ok(())
    }

    fn execute_async_with_loader<F, Fut, T>(f: F) -> Result<T>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        // Use a dedicated thread to ensure we can block even if called from
        // within a single-threaded runtime context (where block_in_place panics).
        let handle = tokio::runtime::Handle::try_current().ok();

        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let res = if let Some(rt) = handle {
                rt.block_on(f())
            } else {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt.block_on(f()),
                    Err(e) => Err(anyhow::anyhow!("failed to create async runtime: {}", e)),
                }
            };
            let _ = tx.send(res);
        })
        .join()
        .map_err(|_| anyhow::anyhow!("MCP worker thread panicked"))?;

        rx.recv()
            .map_err(|e| anyhow::anyhow!("failed to receive result from MCP worker: {}", e))?
    }
}

/// Test connection to a transport by attempting to list tools
async fn test_connection(transport: McpTransport) -> Result<()> {
    let _ = list_tools_via_transport(transport).await?;
    Ok(())
}

async fn list_tools_via_transport(transport: McpTransport) -> Result<Vec<Tool>> {
    match transport {
        McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let cmd_name = Path::new(&command)
                .file_name()
                .map(|s| s.to_string_lossy())
                .unwrap_or_else(|| "unknown".into());
            let log_filename = format!("mcp_server_{}.log", cmd_name);

            // Use sh wrapper to force stderr redirection, bypassing rmcp's potential overrides
            let mut cmd = Command::new("sh");
            cmd.arg("-c");
            cmd.arg(format!("exec \"$@\" 2>> \"{}\"", log_filename));
            cmd.arg("--");
            cmd.arg(&command);
            for arg in &args {
                cmd.arg(arg);
            }

            if let Some(dir) = cwd {
                cmd.current_dir(dir);
            }
            for (key, value) in &env {
                cmd.env(key, value);
            }

            let service = ().serve(TokioChildProcess::new(cmd)?).await?;
            let ListToolsResult { tools, .. } = service.list_tools(None).await?;
            let _ = service.cancel().await;
            Ok(tools)
        }
        McpTransport::Sse { .. } => {
            // SseClientTransport is not supported in rmcp 0.14.0
            anyhow::bail!("SSE transport is not supported in rmcp 0.14.0")
        }
        McpTransport::Http {
            url,
            auth_header,
            allow_stateless,
        } => {
            let mut config = StreamableHttpClientTransportConfig::with_uri(Arc::from(url.as_str()));
            if let Some(header) = auth_header {
                config = config.auth_header(header.clone());
            }
            if let Some(allow) = allow_stateless {
                config.allow_stateless = allow;
            }

            let transport = StreamableHttpClientTransport::from_config(config);
            let service = ().serve(transport).await?;
            let ListToolsResult { tools, .. } = service.list_tools(None).await?;
            let _ = service.cancel().await;
            Ok(tools)
        }
    }
}

async fn call_tool_via_transport(
    transport: McpTransport,
    tool_name: &str,
    arguments: Option<Map<String, Value>>,
) -> Result<CallToolResult> {
    match transport {
        McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let cmd_name = Path::new(&command)
                .file_name()
                .map(|s| s.to_string_lossy())
                .unwrap_or_else(|| "unknown".into());
            let log_filename = format!("mcp_server_{}.log", cmd_name);

            // Use sh wrapper to force stderr redirection, bypassing rmcp's potential overrides
            let mut cmd = Command::new("sh");
            cmd.arg("-c");
            cmd.arg(format!("exec \"$@\" 2>> \"{}\"", log_filename));
            cmd.arg("--");
            cmd.arg(&command);
            for arg in &args {
                cmd.arg(arg);
            }

            if let Some(dir) = cwd {
                cmd.current_dir(dir);
            }
            for (key, value) in &env {
                cmd.env(key, value);
            }

            let service = ().serve(TokioChildProcess::new(cmd)?).await?;
            let response = service
                .call_tool(CallToolRequestParams {
                    name: tool_name.to_string().into(),
                    arguments,
                    meta: None,
                    task: None,
                })
                .await?;
            let _ = service.cancel().await;

            Ok(response)
        }
        McpTransport::Sse { .. } => {
            anyhow::bail!("SSE transport is not supported in rmcp 0.14.0")
        }
        McpTransport::Http {
            url,
            auth_header,
            allow_stateless,
        } => {
            let mut config = StreamableHttpClientTransportConfig::with_uri(Arc::from(url.as_str()));
            if let Some(header) = auth_header {
                config = config.auth_header(header.clone());
            }
            if let Some(allow) = allow_stateless {
                config.allow_stateless = allow;
            }

            let transport = StreamableHttpClientTransport::from_config(config);
            let service = ().serve(transport).await?;
            let response = service
                .call_tool(CallToolRequestParams {
                    name: tool_name.to_string().into(),
                    arguments,
                    meta: None,
                    task: None,
                })
                .await?;
            let _ = service.cancel().await;

            Ok(response)
        }
    }
}

fn sanitize_identifier(input: &str) -> String {
    let mut result: String = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();

    if result.is_empty() {
        result.push('x');
    }

    while result.contains("__") {
        result = result.replace("__", "_");
    }

    let trimmed = result.trim_matches('_');
    if trimmed.is_empty() {
        "mcp_tool".to_string()
    } else {
        trimmed.to_string()
    }
}

fn unique_name(base: &str, set: &mut HashSet<String>) -> String {
    if !set.contains(base) {
        set.insert(base.to_string());
        return base.to_string();
    }

    let mut counter = 2;
    loop {
        let candidate = format!("{base}_{counter}");
        if set.insert(candidate.clone()) {
            return candidate;
        }
        counter += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_server(label: &str) -> McpServer {
        McpServer {
            label: label.to_string(),
            description: Some(format!("{label} server")),
            transport: McpTransport::Sse {
                url: format!("https://example.com/{label}"),
            },
            tools: Vec::new(),
        }
    }

    fn mock_config(label: &str) -> McpServerConfig {
        McpServerConfig {
            label: label.to_string(),
            description: Some(format!("{label} server")),
            transport: McpTransport::Sse {
                url: format!("https://example.com/{label}"),
            },
        }
    }

    #[test]
    fn test_remove_server_cleans_related_state() {
        let mut manager = McpManager::default();
        manager.servers.push(mock_server("alpha"));
        manager.servers.push(mock_server("beta"));

        manager.bindings.insert(
            "mcp__alpha__tool".to_string(),
            ToolBinding {
                server_label: "alpha".to_string(),
                tool_name: "tool".to_string(),
                function_name: "mcp__alpha__tool".to_string(),
            },
        );
        manager.bindings.insert(
            "mcp__beta__tool".to_string(),
            ToolBinding {
                server_label: "beta".to_string(),
                tool_name: "tool".to_string(),
                function_name: "mcp__beta__tool".to_string(),
            },
        );

        manager.session_meta.write().unwrap().insert(
            "alpha".to_string(),
            SessionMeta {
                connected_at: Instant::now(),
            },
        );
        manager
            .connection_errors
            .write()
            .unwrap()
            .insert("alpha".to_string(), "error".to_string());

        assert!(manager.remove_server("alpha"));
        assert_eq!(manager.server_count(), 1);
        assert_eq!(manager.servers[0].label, "beta");
        assert!(
            !manager
                .bindings
                .values()
                .any(|binding| binding.server_label == "alpha")
        );
        assert!(!manager.session_meta.read().unwrap().contains_key("alpha"));
        assert!(
            !manager
                .connection_errors
                .read()
                .unwrap()
                .contains_key("alpha")
        );
    }

    #[test]
    fn test_sync_servers_blocking_ignores_order_only() {
        let mut manager = McpManager::default();
        manager.servers.push(mock_server("alpha"));
        manager.servers.push(mock_server("beta"));

        let stats = manager.sync_servers_blocking(vec![mock_config("beta"), mock_config("alpha")]);

        assert_eq!(
            stats,
            McpSyncStats {
                removed: 0,
                updated: 0,
                added: 0,
                unchanged: 2
            }
        );
        assert_eq!(manager.server_count(), 2);
    }

    #[test]
    fn test_sync_servers_blocking_removes_missing_servers() {
        let mut manager = McpManager::default();
        manager.servers.push(mock_server("alpha"));
        manager.servers.push(mock_server("beta"));

        let stats = manager.sync_servers_blocking(vec![mock_config("beta")]);

        assert_eq!(
            stats,
            McpSyncStats {
                removed: 1,
                updated: 0,
                added: 0,
                unchanged: 1
            }
        );
        assert_eq!(manager.server_count(), 1);
        assert_eq!(manager.servers[0].label, "beta");
    }

    #[test]
    fn test_runtime_state_snapshot_restore() {
        let manager = McpManager::default();
        manager.session_meta.write().unwrap().insert(
            "alpha".to_string(),
            SessionMeta {
                connected_at: Instant::now(),
            },
        );
        manager
            .connection_errors
            .write()
            .unwrap()
            .insert("beta".to_string(), "network error".to_string());

        let snapshot = manager.snapshot_runtime_state();

        manager.session_meta.write().unwrap().clear();
        manager
            .connection_errors
            .write()
            .unwrap()
            .insert("gamma".to_string(), "temporary mutation".to_string());

        manager.restore_runtime_state(snapshot.clone());

        assert_eq!(manager.snapshot_runtime_state(), snapshot);
    }

    #[tokio::test]
    async fn test_sse_transport_error() {
        let transport = McpTransport::Sse {
            url: "http://localhost:8080/sse".to_string(),
        };

        // Test list_tools_via_transport
        let result = list_tools_via_transport(transport.clone()).await;
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "SSE transport is not supported in rmcp 0.14.0"
        );

        // Test call_tool_via_transport
        let result = call_tool_via_transport(transport, "test_tool", None).await;
        // Verify the specific error message
        match result {
            Err(e) => assert_eq!(
                e.to_string(),
                "SSE transport is not supported in rmcp 0.14.0"
            ),
            Ok(_) => panic!("Expected SSE transport to fail"),
        }
    }
}
