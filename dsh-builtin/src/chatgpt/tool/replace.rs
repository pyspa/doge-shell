use crate::safety_policy;
use crate::shell_capabilities::ChatToolHost;
use serde_json::{Value, json};
use std::fs;

pub(crate) const NAME: &str = "str_replace";

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Replace an exact string inside an existing workspace file. Prefer this over `edit` for changing part of a file: `edit` rewrites the file whole. `old_string` must match the file byte for byte, including indentation, and must be unique unless `replace_all` is true.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to modify (relative to current directory or absolute for skills)"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "Exact text to replace. Include enough surrounding lines to make it unique."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Replacement text. Pass an empty string to delete the matched text."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace every occurrence instead of requiring a unique match. Defaults to false."
                    }
                },
                "required": ["path", "old_string", "new_string"],
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn run(arguments: &str, proxy: &mut dyn ChatToolHost) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for str_replace tool: {err}"))?;

    let path_value = parsed
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: str_replace tool requires `path`".to_string())?;

    if path_value.trim().is_empty() {
        return Err("chat: str_replace tool path must not be empty".to_string());
    }

    let old_string = parsed
        .get("old_string")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: str_replace tool requires `old_string`".to_string())?;

    if old_string.is_empty() {
        return Err(
            "chat: str_replace tool `old_string` must not be empty; use edit to create a file"
                .to_string(),
        );
    }

    let new_string = parsed
        .get("new_string")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: str_replace tool requires `new_string`".to_string())?;

    if old_string == new_string {
        return Err(
            "chat: str_replace tool `old_string` and `new_string` are identical".to_string(),
        );
    }

    let replace_all = parsed
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let normalized_abs_path = super::resolve_tool_path(path_value, proxy)?;

    let current_dir = proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;
    let normalized_current_dir =
        std::fs::canonicalize(&current_dir).unwrap_or_else(|_| super::normalize_path(&current_dir));

    super::reject_gitignored_path(&normalized_abs_path, &normalized_current_dir, path_value)?;

    if !normalized_abs_path.is_file() {
        return Err(format!(
            "chat: str_replace tool requires an existing file; `{path_value}` is not one. Use edit to create it."
        ));
    }

    let contents = fs::read_to_string(&normalized_abs_path)
        .map_err(|err| format!("chat: failed to read file `{path_value}`: {err}"))?;

    // Report the mismatch back to the model instead of guessing: the loop feeds
    // tool errors to the assistant so it can retry with a corrected match.
    let matches = contents.matches(old_string).count();
    match matches {
        0 => {
            return Err(format!(
                "chat: str_replace found no match for `old_string` in `{path_value}`. Read the file again and copy the exact text, including indentation."
            ));
        }
        1 => {}
        found if !replace_all => {
            return Err(format!(
                "chat: str_replace found {found} matches for `old_string` in `{path_value}`. Add surrounding context to make it unique, or pass replace_all=true."
            ));
        }
        _ => {}
    }

    // Safety Guard: same confirmation contract as the edit tool.
    let sensitive_note = if safety_policy::is_sensitive_path(&normalized_abs_path)
        || safety_policy::contains_sensitive_text(new_string)
    {
        " Sensitive path or content detected."
    } else {
        ""
    };
    let confirm_msg = format!(
        "AI wants to replace {} occurrence(s) in file: `{}`.{}",
        matches, path_value, sensitive_note
    );
    if !super::confirm_agent_action(
        proxy,
        &super::write_approval_key(&normalized_abs_path),
        &confirm_msg,
    )? {
        return Ok("File modification cancelled by user.".to_string());
    }

    let updated = if replace_all {
        contents.replace(old_string, new_string)
    } else {
        contents.replacen(old_string, new_string, 1)
    };

    fs::write(&normalized_abs_path, &updated)
        .map_err(|err| format!("chat: failed to write file `{path_value}`: {err}"))?;

    Ok(format!(
        "str_replace completed: {matches} replacement(s) in {path_value} ({} -> {} bytes)",
        contents.len(),
        updated.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    use crate::test_support::TestShellProxy;

    fn proxy(path: &std::path::Path) -> TestShellProxy {
        TestShellProxy {
            current_dir: path.to_path_buf(),
            confirm_result: true,
            ..TestShellProxy::default()
        }
    }

    #[test]
    fn replaces_a_unique_match() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let mut p = proxy(dir.path());

        let result = run(
            r#"{"path": "a.txt", "old_string": "beta", "new_string": "delta"}"#,
            &mut p,
        )
        .unwrap();

        assert!(result.starts_with("str_replace completed: 1 replacement(s)"));
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "alpha\ndelta\ngamma\n"
        );
    }

    #[test]
    fn rejects_ambiguous_match_without_replace_all() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "x\nx\n").unwrap();
        let mut p = proxy(dir.path());

        let err = run(
            r#"{"path": "a.txt", "old_string": "x", "new_string": "y"}"#,
            &mut p,
        )
        .unwrap_err();

        assert!(err.contains("found 2 matches"), "{err}");
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "x\nx\n"
        );
    }

    #[test]
    fn replace_all_rewrites_every_occurrence() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "x\nx\n").unwrap();
        let mut p = proxy(dir.path());

        run(
            r#"{"path": "a.txt", "old_string": "x", "new_string": "y", "replace_all": true}"#,
            &mut p,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "y\ny\n"
        );
    }

    #[test]
    fn reports_missing_match_to_the_model() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
        let mut p = proxy(dir.path());

        let err = run(
            r#"{"path": "a.txt", "old_string": "omega", "new_string": "beta"}"#,
            &mut p,
        )
        .unwrap_err();

        assert!(err.contains("found no match"), "{err}");
    }

    #[test]
    fn requires_confirmation_before_writing() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
        let mut p = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            confirm_result: false,
            ..TestShellProxy::default()
        };

        let result = run(
            r#"{"path": "a.txt", "old_string": "alpha", "new_string": "beta"}"#,
            &mut p,
        )
        .unwrap();

        assert_eq!(result, "File modification cancelled by user.");
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "alpha\n"
        );
    }

    #[test]
    fn refuses_to_create_a_missing_file() {
        let dir = tempdir().unwrap();
        let mut p = proxy(dir.path());

        let err = run(
            r#"{"path": "missing.txt", "old_string": "a", "new_string": "b"}"#,
            &mut p,
        )
        .unwrap_err();

        assert!(err.contains("Use edit to create it"), "{err}");
    }

    #[test]
    fn rejects_path_outside_workspace() {
        let dir = tempdir().unwrap();
        let mut p = proxy(dir.path());

        let err = run(
            r#"{"path": "../escape.txt", "old_string": "a", "new_string": "b"}"#,
            &mut p,
        )
        .unwrap_err();

        assert!(err.contains("outside allowed directories"), "{err}");
    }
}
