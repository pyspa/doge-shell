use anyhow::Result;
use dsh_types::mcp::{McpServerConfig, McpTransport};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParam, CallToolResult, ListToolsResult, Tool},
    transport::{
        child_process::TokioChildProcess,
        sse_client::SseClientTransport,
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
    pub fn load(runtime_servers: Vec<McpServerConfig>) -> Self {
        match Self::build_from_servers(runtime_servers) {
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

        match Self::block_on(async move { test_connection(transport).await }) {
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

        let result = Self::block_on(async move {
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

    fn build_from_servers(runtime_servers: Vec<McpServerConfig>) -> Result<Self> {
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
        let results: Vec<Result<(McpServerConfig, Vec<Tool>, bool), (String, anyhow::Error)>> =
            Self::block_on(async move {
                let mut outputs = Vec::new();
                for (i, future) in futures.into_iter().enumerate() {
                    // Add a small delay between server loads to yield CPU/IO to the main thread
                    if i > 0 {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                    outputs.push(future.await);
                }
                Ok(outputs)
            })?;

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

    pub fn add_server(&mut self, config: McpServerConfig) -> Result<()> {
        // Since we are modifying self in place, checking for duplicates against self.servers is needed
        if self.servers.iter().any(|s| s.label == config.label) {
            return Err(anyhow::anyhow!(
                "MCP server `{}` already exists",
                config.label
            ));
        }

        let McpServerConfig {
            label,
            description,
            transport,
        } = config;

        let tools = Self::block_on(list_tools_via_transport(transport.clone()))
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
            tools,
        };

        // Update bindings
        let mut used_names: HashSet<String> = self.bindings.keys().cloned().collect();

        for tool in &server_struct.tools {
            let base_name = format!(
                "mcp__{}__{}",
                sanitize_identifier(&server_struct.label),
                sanitize_identifier(tool.name.as_ref())
            );
            let function_name = unique_name(&base_name, &mut used_names);
            self.bindings.insert(
                function_name.clone(),
                ToolBinding {
                    server_label: server_struct.label.clone(),
                    tool_name: tool.name.to_string(),
                    function_name,
                },
            );
        }

        self.servers.push(server_struct);

        Ok(())
    }

    fn block_on<F, T>(future: F) -> Result<T>
    where
        F: std::future::Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        // Use a dedicated thread to ensure we can block even if called from
        // within a single-threaded runtime context (where block_in_place panics).
        let handle = tokio::runtime::Handle::try_current().ok();

        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let res = if let Some(rt) = handle {
                rt.block_on(future)
            } else {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt.block_on(future),
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
        McpTransport::Sse { url } => {
            let transport = SseClientTransport::start(Arc::from(url.as_str())).await?;
            let service = ().serve(transport).await?;
            let ListToolsResult { tools, .. } = service.list_tools(None).await?;
            let _ = service.cancel().await;
            Ok(tools)
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
                .call_tool(CallToolRequestParam {
                    name: tool_name.to_string().into(),
                    arguments,
                })
                .await?;
            let _ = service.cancel().await;

            Ok(response)
        }
        McpTransport::Sse { url } => {
            let transport = SseClientTransport::start(Arc::from(url.as_str())).await?;
            let service = ().serve(transport).await?;
            let response = service
                .call_tool(CallToolRequestParam {
                    name: tool_name.to_string().into(),
                    arguments,
                })
                .await?;
            let _ = service.cancel().await;

            Ok(response)
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
                .call_tool(CallToolRequestParam {
                    name: tool_name.to_string().into(),
                    arguments,
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
