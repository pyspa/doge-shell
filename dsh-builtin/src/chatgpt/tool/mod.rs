use serde_json::Value;

use super::mcp::McpManager;
use crate::ShellProxy;

mod edit;
mod execute;

pub fn build_tools() -> Vec<Value> {
    vec![edit::definition(), execute::definition()]
}

pub fn execute_tool_call(
    tool_call: &Value,
    mcp: &McpManager,
    proxy: &mut dyn ShellProxy,
) -> Result<String, String> {
    let function = tool_call
        .get("function")
        .ok_or_else(|| "chat: tool call missing function".to_string())?;

    let name = function
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: tool call missing function name".to_string())?;

    let arguments = function
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    if let Some(result) = mcp.execute_tool(name, arguments)? {
        return Ok(result);
    }

    match name {
        edit::NAME => edit::run(arguments, proxy),
        execute::NAME => execute::run(arguments, proxy),
        other => Err(format!("chat: unsupported tool `{other}`")),
    }
}
