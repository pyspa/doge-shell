use crate::ShellProxy;
use anyhow::Result;
use serde_json::{Value, json};
use std::fs;

pub(crate) const NAME: &str = "edit";

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Create or overwrite a workspace file with the provided contents.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit (relative to current directory or absolute for skills)"
                    },
                    "contents": {
                        "type": "string",
                        "description": "Full desired contents of the file. The file will be overwritten with this value."
                    }
                },
                "required": ["path", "contents"],
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn run(arguments: &str, proxy: &mut dyn ShellProxy) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for edit tool: {err}"))?;

    let path_value = parsed
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: edit tool requires `path`".to_string())?;

    if path_value.trim().is_empty() {
        return Err("chat: edit tool path must not be empty".to_string());
    }

    let contents = parsed
        .get("contents")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: edit tool requires `contents`".to_string())?;

    let normalized_abs_path = super::resolve_tool_path(path_value, proxy)?;

    // Get CWD for error reporting (if needed) or comparison
    let _current_dir = proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;

    if let Some(parent) = normalized_abs_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|err| format!("chat: failed to create parent directories: {err}"))?;
    }

    // Safety Guard: Request confirmation from user
    let confirm_msg = format!("AI wants to write to file: `{}`. \r\nProceed?", path_value);
    if !proxy
        .confirm_action(&confirm_msg)
        .map_err(|e: anyhow::Error| e.to_string())?
    {
        return Ok("File modification cancelled by user.".to_string());
    }

    fs::write(&normalized_abs_path, contents)
        .map_err(|err| format!("chat: failed to write file `{path_value}`: {err}"))?;

    Ok(format!(
        "edit completed: wrote {} bytes to {}",
        contents.len(),
        path_value
    ))
}
// Helper function to normalize a path by resolving all relative componen
