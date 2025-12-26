use crate::ShellProxy;
use anyhow::Result;
use serde_json::{Value, json};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

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
                        "description": "Relative path to the file to edit (no absolute paths or ..)"
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

    let path = Path::new(path_value);

    if path.is_absolute() {
        return Err("chat: edit tool path must be relative".to_string());
    }

    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("chat: edit tool path must not contain `..`".to_string());
    }

    // Get current working directory
    let current_dir = env::current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;

    // Convert the relative path to an absolute path by joining with current directory
    let abs_path = current_dir.join(path);

    // Normalize the absolute path to resolve any relative components like "." or ".."
    let normalized_abs_path = normalize_path(&abs_path);
    let normalized_current_dir = normalize_path(&current_dir);

    // Check if the resolved path is within the current directory
    if !normalized_abs_path.starts_with(&normalized_current_dir) {
        return Err(format!(
            "chat: edit tool path `{path_value}` resolves outside current directory (resolved to: {})",
            normalized_abs_path.display()
        ));
    }

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

// Helper function to normalize a path by resolving all relative components
fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {
                // Skip current directory components
            }
            _ => {
                normalized.push(component);
            }
        }
    }
    normalized
}
