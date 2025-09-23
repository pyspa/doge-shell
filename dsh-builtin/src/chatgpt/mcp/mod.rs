use anyhow::{Context, Result};
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
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    sync::Arc,
};
use tokio::{process::Command, runtime::Runtime, task};
use tracing::{debug, warn};
use xdg::BaseDirectories;

const APP_NAME: &str = "dsh";
const CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Debug, Default, Deserialize)]
struct ConfigRoot {
    #[serde(default)]
    mcp: Option<McpSection>,
}

#[derive(Debug, Default, Deserialize)]
struct McpSection {
    #[serde(default = "Vec::new")]
    servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone)]
struct McpServer {
    label: String,
    description: Option<String>,
    transport: McpTransport,
    tools: Vec<Tool>,
}

#[derive(Debug, Clone)]
struct ToolBinding {
    server_label: String,
    tool_name: String,
    function_name: String,
}

#[derive(Debug, Default, Clone)]
pub struct McpManager {
    servers: Vec<McpServer>,
    bindings: HashMap<String, ToolBinding>,
    warnings: Vec<String>,
}

impl McpManager {
    pub fn load(runtime_servers: Vec<McpServerConfig>) -> Self {
        match Self::load_internal(runtime_servers) {
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

        let mut lines = Vec::new();
        lines.push(
            "You can call external Model Context Protocol (MCP) servers when solving tasks."
                .to_string(),
        );
        lines.push(
            "Always prefer the dedicated MCP function tools when they cover the action you need."
                .to_string(),
        );
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
        let result =
            Self::block_on(
                async move { call_tool_via_transport(transport, &tool_name, map).await },
            )
            .map_err(|err| format!("failed to call MCP tool: {err}"))?;

        let json = serde_json::to_value(&result)
            .map_err(|err| format!("failed to serialize MCP tool result: {err}"))?;

        Ok(Some(json.to_string()))
    }

    fn load_internal(runtime_servers: Vec<McpServerConfig>) -> Result<Self> {
        let config_text = match read_config_file()? {
            Some(contents) => contents,
            None => {
                return Self::build_from_servers(runtime_servers, Vec::new());
            }
        };

        let config: ConfigRoot =
            toml::from_str(&config_text).context("failed to parse config.toml for MCP settings")?;

        let config_servers = config
            .mcp
            .map(|section| section.servers)
            .unwrap_or_default();
        Self::build_from_servers(runtime_servers, config_servers)
    }

    fn build_from_servers(
        runtime_servers: Vec<McpServerConfig>,
        mut config_servers: Vec<McpServerConfig>,
    ) -> Result<Self> {
        let mut servers = Vec::new();
        let mut labels = HashSet::new();
        let mut warnings = Vec::new();

        let mut combined = runtime_servers;
        combined.append(&mut config_servers);

        for server in combined {
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

            let McpServerConfig {
                label,
                description,
                transport,
            } = server;

            let tools = match Self::block_on(list_tools_via_transport(transport.clone())) {
                Ok(tools) => tools,
                Err(err) => {
                    warnings.push(format!("failed to load MCP server `{}`: {err}", label));
                    continue;
                }
            };

            debug!(
                server = label.as_str(),
                tool_count = tools.len(),
                "registered MCP server"
            );

            servers.push(McpServer {
                label,
                description,
                transport,
                tools,
            });
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
        })
    }

    fn block_on<F, T>(future: F) -> Result<T>
    where
        F: std::future::Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            task::block_in_place(|| handle.block_on(future))
        } else {
            Runtime::new()
                .context("failed to create async runtime for MCP operations")?
                .block_on(future)
        }
    }
}

async fn list_tools_via_transport(transport: McpTransport) -> Result<Vec<Tool>> {
    match transport {
        McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let mut cmd = Command::new(&command);
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
            let mut cmd = Command::new(&command);
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

fn read_config_file() -> Result<Option<String>> {
    let dirs = BaseDirectories::with_prefix(APP_NAME)
        .context("failed to locate configuration directory for MCP")?;

    if let Some(path) = dirs.find_config_file(CONFIG_FILE_NAME) {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(Some(content))
    } else {
        Ok(None)
    }
}
