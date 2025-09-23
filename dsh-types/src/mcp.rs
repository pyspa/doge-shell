use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct McpServerConfig {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    pub transport: McpTransport,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpTransport {
    #[serde(alias = "stdio", alias = "local")]
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        #[serde(default)]
        cwd: Option<PathBuf>,
    },
    #[serde(alias = "sse")]
    Sse { url: String },
    #[serde(
        alias = "http",
        alias = "https",
        alias = "streamable_http",
        alias = "streamable-http"
    )]
    Http {
        url: String,
        #[serde(default)]
        auth_header: Option<String>,
        #[serde(default)]
        allow_stateless: Option<bool>,
    },
}
