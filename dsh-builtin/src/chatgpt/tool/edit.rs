use serde_json::{Value, json};
use std::fs;
use std::path::{Component, Path};

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

pub(crate) fn run(arguments: &str) -> Result<String, String> {
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

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|err| format!("chat: failed to create parent directories: {err}"))?;
    }

    fs::write(path, contents)
        .map_err(|err| format!("chat: failed to write file `{path_value}`: {err}"))?;

    Ok(format!(
        "edit completed: wrote {} bytes to {}",
        contents.len(),
        path_value
    ))
}
