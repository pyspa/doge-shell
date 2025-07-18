use crate::history::FrecencyHistory;
use anyhow::{Context as _, Result, bail};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tracing::debug;

/// Trait for shell history importers
pub trait HistoryImporter {
    /// Import history from the shell's history file
    fn import(&self, history: &mut FrecencyHistory) -> Result<usize>;
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
    fn import(&self, history: &mut FrecencyHistory) -> Result<usize> {
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

        // 変更: バイト単位で読み込み、UTF-8エラーを処理
        let mut reader = BufReader::new(file);
        let mut count = 0;
        let mut in_cmd_block = false;
        let mut current_cmd = String::new();
        let mut line_buffer = Vec::new();
        let mut line_number = 0;

        // Fish history format is:
        // - cmd: <command>
        //   when: <timestamp>
        // - cmd: <another command>
        //   when: <timestamp>
        while let Ok(bytes_read) = reader.read_until(b'\n', &mut line_buffer) {
            if bytes_read == 0 {
                break; // ファイルの終端に達した
            }

            line_number += 1;

            // 無効なUTF-8シーケンスを置換文字に変換
            let line = String::from_utf8_lossy(&line_buffer).into_owned();

            // 元のバイト列に無効なUTF-8シーケンスが含まれていた場合、警告ログを出力
            if line.contains('�') {
                tracing::warn!(
                    "Line {line_number} contains invalid UTF-8 characters, replaced with '�'"
                );
            }

            let trimmed = line.trim();

            if trimmed.starts_with("- cmd:") {
                // Start of a new command entry
                if in_cmd_block && !current_cmd.is_empty() {
                    // Add the previous command if we were in a command block
                    history.add(&current_cmd);
                    count += 1;
                }

                // Extract the command part
                let cmd_part = trimmed.strip_prefix("- cmd:").unwrap_or("").trim();
                current_cmd = cmd_part.to_string();
                in_cmd_block = true;
            } else if trimmed.starts_with("when:") && in_cmd_block {
                // End of command block, add the command to history
                if !current_cmd.is_empty() {
                    history.add(&current_cmd);
                    count += 1;
                    current_cmd.clear();
                }
                in_cmd_block = false;
            }

            // バッファをクリアして次の行の読み込み準備
            line_buffer.clear();
        }

        // Add the last command if we were still in a command block
        if in_cmd_block && !current_cmd.is_empty() {
            history.add(&current_cmd);
            count += 1;
        }

        // 明示的にchangedフラグを設定して保存を強制
        history.force_changed();

        // Save the imported history
        if let Err(err) = history.save() {
            tracing::error!("Failed to save imported history: {err}");
            return Err(err.context("Failed to save imported history"));
        }

        // 保存後に履歴ファイルのサイズを確認
        if let Some(ref path) = history.path {
            if let Ok(metadata) = std::fs::metadata(path) {
                tracing::debug!(
                    "History file saved: {} (size: {} bytes)",
                    path.display(),
                    metadata.len()
                );
            }
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
        let dsh_history_path = temp_dir.path().join("dsh_history");
        let mut history = FrecencyHistory::new();
        history.path = Some(dsh_history_path);

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
        let dsh_history_path = temp_dir.path().join("dsh_history");
        let mut history = FrecencyHistory::new();
        history.path = Some(dsh_history_path);

        // Import the fish history - 無効なUTF-8があっても成功するはず
        let importer = FishHistoryImporter::with_path(&history_path);
        let count = importer.import(&mut history)?;

        // 3つのコマンドがインポートされるはず
        assert_eq!(count, 3);

        Ok(())
    }
}
