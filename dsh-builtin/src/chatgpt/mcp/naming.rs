//! The edges around a server process: probing a transport for its tool list,
//! deriving the stable function names the model sees, spawning the stdio child
//! with its stderr routed to a per-command log, and rendering a tool result
//! down to the text the model should actually read.
use super::*;

/// Test connection to a transport by attempting to list tools
pub(super) async fn test_connection(transport: McpTransport) -> Result<()> {
    let _ = list_tools_via_transport(transport).await?;
    Ok(())
}

pub(super) async fn list_tools_via_transport(transport: McpTransport) -> Result<Vec<Tool>> {
    timeout(DEFAULT_TOOL_TIMEOUT, async {
        let service = connection::open(transport).await?;
        let result = service.list_all_tools().await;
        let _ = timeout(Duration::from_secs(1), service.cancel()).await;
        result.map_err(anyhow::Error::from)
    })
    .await
    .map_err(|_| anyhow::anyhow!("MCP discovery timed out"))?
}

pub(super) fn sanitize_identifier(input: &str) -> String {
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

pub(super) fn stable_name(base: &str, label: &str, tool: &str) -> String {
    if sanitize_identifier(label) == label
        && sanitize_identifier(tool) == tool
        && !label.contains("__")
        && !tool.contains("__")
        && base.len() <= 64
    {
        return base.to_owned();
    }
    // FNV-1a is deterministic across builds and processes (DefaultHasher isn't a storage format).
    let mut hash = 0xcbf29ce484222325u64;
    for byte in label.bytes().chain([0]).chain(tool.bytes()) {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let prefix = &base[..base.floor_char_boundary(base.len().min(44))];
    format!("{prefix}__h_{hash:016x}")
}

pub(super) fn spawn_mcp_stdio_transport(
    cmd: Command,
    command: &str,
) -> std::io::Result<TokioChildProcess> {
    let (transport, _) = spawn_mcp_stdio_transport_with_stderr(cmd, mcp_server_stderr(command))?;
    Ok(transport)
}

pub(super) fn spawn_mcp_stdio_transport_with_stderr(
    cmd: Command,
    stderr: Stdio,
) -> std::io::Result<(TokioChildProcess, Option<tokio::process::ChildStderr>)> {
    TokioChildProcess::builder(cmd).stderr(stderr).spawn()
}

pub(super) fn mcp_server_stderr(command: &str) -> Stdio {
    let Some(path) = mcp_server_log_path(command) else {
        warn!(command, "failed to resolve MCP server stderr log path");
        return Stdio::null();
    };
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => Stdio::from(file),
        Err(err) => {
            warn!(command, path = %path.display(), "failed to open MCP server stderr log: {err}");
            Stdio::null()
        }
    }
}

pub(super) fn mcp_server_log_path(command: &str) -> Option<PathBuf> {
    BaseDirectories::with_prefix("dogesh")
        .place_cache_file(format!(
            "mcp_server_{}.log",
            sanitized_command_name(command)
        ))
        .ok()
}

pub(super) fn sanitized_command_name(command: &str) -> String {
    let name = Path::new(command)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown");
    let sanitized = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

/// Budget for one MCP tool result, applied before the shared tool-output cap.
pub(super) const MAX_TOOL_RESULT_CHARS: usize = 6144;

/// Turn a `CallToolResult` into what the model should actually read.
///
/// The whole struct used to be serialized and handed on as JSON, which the
/// shared cap then truncated *from the middle* - so a large result reached the
/// model as a JSON document with a hole in it, unparseable and impossible to
/// act on. Almost all of the value is in the text content anyway; the wrapper
/// was paid for on every call and read by nobody.
pub(super) fn render_tool_result(result: &rmcp::model::CallToolResult) -> Result<String, String> {
    let mut text = String::new();

    for content in &result.content {
        if let Some(raw) = content.as_text() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&raw.text);
        }
    }

    if text.trim().is_empty() {
        // No text at all - an image, a resource link, or a structured payload.
        // Fall back to JSON rather than reporting an empty result.
        let json = serde_json::to_value(result)
            .map_err(|err| format!("failed to serialize MCP tool result: {err}"))?;
        text = json.to_string();
    }

    if result.is_error.unwrap_or(false) {
        text = format!("The tool reported an error:\n{text}");
    }

    Ok(dsh_openai::turn::truncate_middle(
        &text,
        MAX_TOOL_RESULT_CHARS,
    ))
}
