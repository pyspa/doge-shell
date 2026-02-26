use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct McpServerConfig {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    pub transport: McpTransport,
}

impl fmt::Debug for McpServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServerConfig")
            .field("label", &self.label)
            .field("description", &self.description)
            .field("transport", &self.transport)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
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

impl fmt::Debug for McpTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            McpTransport::Stdio {
                command,
                args,
                env,
                cwd,
            } => f
                .debug_struct("Stdio")
                .field("command", command)
                .field("args", args)
                .field("env", env)
                .field("cwd", cwd)
                .finish(),
            McpTransport::Sse { url } => f.debug_struct("Sse").field("url", url).finish(),
            McpTransport::Http {
                url,
                auth_header,
                allow_stateless,
            } => {
                let redacted = auth_header.as_ref().map(|_| "<redacted>");
                f.debug_struct("Http")
                    .field("url", url)
                    .field("auth_header", &redacted)
                    .field("allow_stateless", allow_stateless)
                    .finish()
            }
        }
    }
}
