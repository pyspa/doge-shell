use anyhow::{Context, Result};
use dsh_types::process_runtime::CommandRuntimeSnapshot;
use std::fs::File;
use std::io::{Read, Write};
use tempfile::Builder;

/// Resolve the editor command line from logical shell state.
///
/// Precedence is unchanged: logical `VISUAL`, then logical `EDITOR`, then
/// the first available fallback (`emacsclient -nw`, `vim`, `nano`).
/// Fallback existence is checked against the runtime snapshot's logical
/// `PATH`, never the process-global one. Blank values count as unset.
pub fn editor_command_for(
    snapshot: &CommandRuntimeSnapshot,
    visual: Option<&str>,
    editor: Option<&str>,
) -> String {
    let configured = visual
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| editor.map(str::trim).filter(|value| !value.is_empty()));
    if let Some(configured) = configured {
        return configured.to_string();
    }
    if snapshot.resolve_bare_program("emacsclient").is_some() {
        "emacsclient -nw".to_string()
    } else if snapshot.resolve_bare_program("vim").is_some() {
        "vim".to_string()
    } else {
        "nano".to_string()
    }
}

/// Launch the configured editor for the given path.
///
/// `visual`/`editor` are the logical `VISUAL`/`EDITOR` shell variables
/// (exported or not — they configure the shell, not the child). The
/// executable itself is resolved through the snapshot, and the child
/// inherits exactly the snapshot's exported environment.
pub fn launch_editor(
    snapshot: &CommandRuntimeSnapshot,
    visual: Option<&str>,
    editor: Option<&str>,
    path: &std::path::Path,
) -> Result<()> {
    // Determine the editor command
    // Order: $VISUAL -> $EDITOR -> emacsclient -nw -> vim -> nano
    let editor_cmd = editor_command_for(snapshot, visual, editor);

    // Validated launch: keep the existing whitespace split, so editor
    // commands with arguments (`emacsclient -nw`, `code -n`) keep working.
    // Only the executable resolution moved to the runtime authority.
    let parts: Vec<&str> = editor_cmd.split_whitespace().collect();
    if parts.is_empty() {
        return Err(anyhow::anyhow!("No editor found"));
    }

    let status = snapshot
        .std_command(parts[0])
        .ok_or_else(|| anyhow::anyhow!("No editor found"))?
        .args(&parts[1..])
        .arg(path)
        .status()
        .context("Failed to launch editor")?;

    if !status.success() {
        return Err(anyhow::anyhow!("Editor exited with non-zero status"));
    }
    Ok(())
}

/// Open content in an external editor and return the modified content.
///
/// # Arguments
///
/// * `content` - The initial content to populate the file with.
/// * `extension` - The file extension to use for the temporary file (e.g., "sh", "txt").
///
/// # Returns
///
/// * `Result<String>` - The modified content after the editor is closed.
pub fn open_editor(
    snapshot: &CommandRuntimeSnapshot,
    visual: Option<&str>,
    editor: Option<&str>,
    content: &str,
    extension: &str,
) -> Result<String> {
    // 1. Create a temporary file
    let mut temp_file = Builder::new()
        .prefix("doge_edit_")
        .suffix(&format!(".{}", extension))
        .tempfile()?;

    // 2. Write content to the temporary file
    temp_file.write_all(content.as_bytes())?;
    let temp_path = temp_file.path().to_path_buf();

    // 3. Launch the editor
    launch_editor(snapshot, visual, editor, &temp_path)?;

    // 4. Read the modified content back
    let mut modified_content = String::new();
    let mut file = File::open(&temp_path)?;
    file.read_to_string(&mut modified_content)?;

    // Trim trailing newline added by some editors if it wasn't there before?
    // Usually shells execute exactly what's in the file.
    // However, editors usually add a newline at EOF.
    // If the original content didn't have it, we might want to strip it,
    // but for shell commands, a trailing newline is usually fine or ignored.
    // We'll return as is, trimming only if it's strictly whitespace potentially.
    // But let's just return the file content.
    Ok(modified_content.trim_end_matches('\n').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_snapshot() -> CommandRuntimeSnapshot {
        CommandRuntimeSnapshot::new(Vec::new(), HashMap::new(), std::path::PathBuf::from("/"))
    }

    #[test]
    fn logical_visual_wins_without_touching_process_env() {
        let snapshot = empty_snapshot();
        assert_eq!(
            editor_command_for(&snapshot, Some("my-visual"), Some("my-editor")),
            "my-visual"
        );
        assert_eq!(
            editor_command_for(&snapshot, None, Some("my-editor")),
            "my-editor"
        );
    }

    #[test]
    fn blank_values_fall_through() {
        let snapshot = empty_snapshot();
        // No logical editor and nothing on the empty logical PATH: nano.
        assert_eq!(editor_command_for(&snapshot, Some("  "), Some("")), "nano");
    }

    #[test]
    fn fallback_prefers_emacsclient_then_vim() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("vim"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = root.path().join("vim");
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        let snapshot = CommandRuntimeSnapshot::new(
            vec![root.path().to_path_buf()],
            HashMap::new(),
            std::path::PathBuf::from("/"),
        );
        assert_eq!(editor_command_for(&snapshot, None, None), "vim");
    }
}
