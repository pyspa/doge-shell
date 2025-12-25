use crate::history::History;
use anyhow::{Context as _, Result, bail};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tracing::debug;

/// Trait for shell history importers
pub trait HistoryImporter {
    /// Import history from the shell's history file
    fn import(&self, history: &mut History) -> Result<usize>;
}

/// Fish shell history importer
pub struct FishHistoryImporter {
    history_path: PathBuf,
}

impl FishHistoryImporter {
    /// Create a new Fish history importer
    ///
    /// By default, it will look for the history file at ~/.local/share/fish/fish_history
    pub fn new() -> Result<Self> {
        let home_dir = dirs::home_dir().context("Failed to get home directory")?;
        let default_path = home_dir.join(".local/share/fish/fish_history");

        if !default_path.exists() {
            bail!("Fish history file not found at {}", default_path.display());
        }

        Ok(Self {
            history_path: default_path,
        })
    }

    /// Create a new Fish history importer with a custom path
    pub fn with_path<P: AsRef<Path>>(path: P) -> Self {
        Self {
            history_path: path.as_ref().to_path_buf(),
        }
    }
}

impl HistoryImporter for FishHistoryImporter {
    fn import(&self, history: &mut History) -> Result<usize> {
        debug!(
            "Importing fish history from {}",
            self.history_path.display()
        );

        let file = File::open(&self.history_path).with_context(|| {
            let error_msg = format!(
                "Failed to open fish history file: {}",
                self.history_path.display()
            );
            tracing::error!("{error_msg}");
            error_msg
        })?;

        let mut reader = BufReader::new(file);
        let mut count = 0;
        let mut in_cmd_block = false;
        let mut current_cmd = String::new();
        let mut line_buffer = Vec::new();
        let mut line_number = 0;
        let mut entries = Vec::new();

        while let Ok(bytes_read) = reader.read_until(b'\n', &mut line_buffer) {
            if bytes_read == 0 {
                break;
            }

            line_number += 1;
            let line = String::from_utf8_lossy(&line_buffer).into_owned();

            if line.contains('\u{FFFD}') {
                tracing::warn!(
                    "Line {line_number} contains invalid UTF-8 characters, replaced with '\\u{{FFFD}}'"
                );
            }

            let trimmed = line.trim();

            if trimmed.starts_with("- cmd:") {
                if in_cmd_block && !current_cmd.is_empty() {
                    // If we see a new - cmd: but didn't see when: yet
                    entries.push((current_cmd.clone(), chrono::Local::now().timestamp()));
                    count += 1;
                }

                let cmd_part = trimmed.strip_prefix("- cmd:").unwrap_or("").trim();
                current_cmd = cmd_part.to_string();
                in_cmd_block = true;
            } else if trimmed.starts_with("when:") && in_cmd_block {
                let when_part = trimmed.strip_prefix("when:").unwrap_or("").trim();
                let when = when_part
                    .parse::<i64>()
                    .unwrap_or_else(|_| chrono::Local::now().timestamp());

                if !current_cmd.is_empty() {
                    entries.push((current_cmd.clone(), when));
                    count += 1;
                    current_cmd.clear();
                }
                in_cmd_block = false;
            }

            line_buffer.clear();
        }

        if in_cmd_block && !current_cmd.is_empty() {
            entries.push((current_cmd, chrono::Local::now().timestamp()));
            count += 1;
        }

        if !entries.is_empty() {
            debug!("Writing {} entries to history...", entries.len());
            history.write_batch(entries)?;
        }

        debug!("Imported {} commands from fish history", count);
        tracing::info!("Successfully imported {count} commands from fish history");
        Ok(count)
    }
}

/// Factory function to create a history importer for the specified shell
pub fn create_importer(
    shell_name: &str,
    custom_path: Option<&str>,
) -> Result<Box<dyn HistoryImporter>> {
    tracing::debug!("Creating history importer for {shell_name} shell");

    if let Some(path) = &custom_path {
        tracing::debug!("Using custom path: {path}");
    }

    match shell_name.to_lowercase().as_str() {
        "fish" => {
            if let Some(path) = custom_path {
                tracing::debug!("Creating Fish history importer with custom path: {path}");
                Ok(Box::new(FishHistoryImporter::with_path(path)))
            } else {
                tracing::debug!("Creating Fish history importer with default path");
                match FishHistoryImporter::new() {
                    Ok(importer) => Ok(Box::new(importer)),
                    Err(err) => {
                        tracing::error!("Failed to create Fish history importer: {err}");
                        Err(err)
                    }
                }
            }
        }
        _ => {
            let error_msg = format!("Unsupported shell: {shell_name}");
            tracing::error!("{error_msg}");
            bail!(error_msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_fish_history_import() -> Result<()> {
        // Create a temporary directory for the test
        let temp_dir = tempdir()?;
        let history_path = temp_dir.path().join("fish_history");

        // Create a mock fish history file
        let mut file = File::create(&history_path)?;
        writeln!(file, "- cmd: ls -la")?;
        writeln!(file, "  when: 1625097600")?;
        writeln!(file, "- cmd: cd /tmp")?;
        writeln!(file, "  when: 1625097601")?;
        writeln!(file, "- cmd: echo hello")?;
        writeln!(file, "  when: 1625097602")?;
        file.flush()?;

        // Create a temporary history file for dsh
        let mut history = History::from_file("dsh_test_import")?;

        // Import the fish history
        let importer = FishHistoryImporter::with_path(&history_path);
        let count = importer.import(&mut history)?;

        // Check that we imported the correct number of commands
        assert_eq!(count, 3);

        Ok(())
    }

    #[test]
    fn test_fish_history_import_with_invalid_utf8() -> Result<()> {
        // Create a temporary directory for the test
        let temp_dir = tempdir()?;
        let history_path = temp_dir.path().join("fish_history");

        // Create a mock fish history file with invalid UTF-8 sequence
        let mut file = File::create(&history_path)?;

        // 通常の行
        writeln!(file, "- cmd: ls -la")?;
        writeln!(file, "  when: 1625097600")?;

        // 無効なUTF-8シーケンスを含む行
        // 0x80, 0x90は単独では無効なUTF-8シーケンス
        file.write_all(b"- cmd: echo \x80\x90 invalid utf8\n")?;
        writeln!(file, "  when: 1625097601")?;

        // 通常の行
        writeln!(file, "- cmd: echo hello")?;
        writeln!(file, "  when: 1625097602")?;

        file.flush()?;

        // Create a temporary history file for dsh
        let _dsh_history_path = temp_dir.path().join("dsh_history");
        let mut history = History::from_file("dsh_test_import")?;

        // Import the fish history - 無効なUTF-8があっても成功するはず
        let importer = FishHistoryImporter::with_path(&history_path);
        let count = importer.import(&mut history)?;

        // 3つのコマンドがインポートされるはず
        assert_eq!(count, 3);

        Ok(())
    }
}
