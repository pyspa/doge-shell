use anyhow::{Context, Result};
use std::env;
use std::fs::File;
use std::io::{Read, Write};
use std::process::Command;
use tempfile::Builder;

/// Launch the configured editor for the given path.
pub fn launch_editor(path: &std::path::Path) -> Result<()> {
    // Determine the editor command
    // Order: $VISUAL -> $EDITOR -> emacsclient -nw -> vim -> nano
    let editor_cmd = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| {
            if which::which("emacsclient").is_ok() {
                "emacsclient -nw".to_string()
            } else if which::which("vim").is_ok() {
                "vim".to_string()
            } else {
                "nano".to_string()
            }
        });

    // Validated launch
    let parts: Vec<&str> = editor_cmd.split_whitespace().collect();
    if parts.is_empty() {
        return Err(anyhow::anyhow!("No editor found"));
    }

    let status = Command::new(parts[0])
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
pub fn open_editor(content: &str, extension: &str) -> Result<String> {
    // 1. Create a temporary file
    let mut temp_file = Builder::new()
        .prefix("doge_edit_")
        .suffix(&format!(".{}", extension))
        .tempfile()?;

    // 2. Write content to the temporary file
    temp_file.write_all(content.as_bytes())?;
    let temp_path = temp_file.path().to_path_buf();

    // 3. Launch the editor
    launch_editor(&temp_path)?;

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
