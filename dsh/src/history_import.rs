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
    /// Look for the default fish session under the XDG data directory.
    pub fn new() -> Result<Self> {
        let default_path = xdg::BaseDirectories::with_prefix("fish")
            .get_data_home()
            .context("Failed to resolve fish data directory")?
            .join("fish_history");

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
        let mut current_cmd = None;
        let mut line_buffer = Vec::new();
        let mut line_number = 0;
        let mut entries = Vec::new();

        loop {
            let bytes_read = reader
                .read_until(b'\n', &mut line_buffer)
                .with_context(|| {
                    format!(
                        "Failed to read fish history file: {}",
                        self.history_path.display()
                    )
                })?;
            if bytes_read == 0 {
                break;
            }

            line_number += 1;
            let line = line_buffer.strip_suffix(b"\n").unwrap_or(&line_buffer);
            let line = line.trim_ascii_start();

            if let Some(raw) = line.strip_prefix(b"- cmd:") {
                if let Some(previous) = current_cmd.take() {
                    entries.push((previous, None));
                }
                // Fish inserts one space after the colon. All remaining
                // bytes, including leading and trailing spaces, are command data.
                let raw = raw.strip_prefix(b" ").unwrap_or(raw);
                if !raw.is_empty() {
                    if std::str::from_utf8(raw).is_err() {
                        tracing::warn!("Invalid UTF-8 in fish command on line {line_number}");
                    }
                    current_cmd = Some(unescape_fish_command(raw));
                }
            } else if let Some(raw) = line.strip_prefix(b"when:")
                && let Some(mut command) = current_cmd.take()
            {
                // Native fish uses LF and can store a literal trailing CR.
                // A CR on the timestamp line identifies a CRLF-converted record.
                if raw.ends_with(b"\r") && command.last() == Some(&b'\r') {
                    command.pop();
                }
                let when = std::str::from_utf8(raw.trim_ascii())
                    .ok()
                    .and_then(|value| value.parse().ok());
                if !command.is_empty() {
                    entries.push((command, when));
                }
            }

            line_buffer.clear();
        }

        if let Some(command) = current_cmd {
            entries.push((command, None));
        }

        let count = history.import_fish_entries(entries)?;
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

/// Undo fish's history escaping: fish writes `\` as `\\` and a newline
/// inside a command as `\n`. Any other backslash pair is kept as written.
fn unescape_fish_command(escaped: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(escaped.len());
    let mut bytes = escaped.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        match bytes.next() {
            Some(b'\\') => out.push(b'\\'),
            Some(b'n') => out.push(b'\n'),
            Some(other) => out.extend([b'\\', other]),
            None => out.push(b'\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fish_escapes_for_backslash_and_newline_are_undone() {
        assert_eq!(unescape_fish_command(br"echo a\\b"), br"echo a\b");
        assert_eq!(
            unescape_fish_command(br"for x in 1\n  echo $x\nend"),
            b"for x in 1\n  echo $x\nend"
        );
        assert_eq!(unescape_fish_command(br"printf '\\n'"), br"printf '\n'");
        assert_eq!(
            unescape_fish_command(br"odd\t trailing\"),
            br"odd\t trailing\"
        );
    }

    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn fish_import_reports_read_errors() {
        let dir = tempdir().unwrap();
        let importer = FishHistoryImporter::with_path(dir.path());
        assert!(importer.import(&mut History::new()).is_err());
    }

    #[test]
    fn fish_import_aggregates_unsorted_occurrences_without_losing_newer_history() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path().join("fish_history");
        std::fs::write(
            &path,
            "- cmd: Hit old\n  when: 10\n- cmd: Hit imported\n  when: 90\n\
             - cmd: Hit first tie\n  when: 100\n- cmd: Hit imported\n  when: 5\n\
             - cmd: Hit last tie\n  when: 100\n- cmd: Hit first tie\n  when: 100\n",
        )?;
        let mut history = History::new();
        let db = crate::db::Db::new(dir.path().join("commands.db"))?;
        db.get_connection().execute_batch(
            "INSERT INTO command_history(command, timestamp, count, context, exit_code,
                 duration_ms, cwd, session_id, hostname)
             VALUES ('Hit old', 100, 4, '/project', 7, 123, '/work', 'session', 'host');
             INSERT INTO command_history(command, timestamp) VALUES ('Resident', 100);",
        )?;
        history.db = Some(db);
        // The command with metadata sits just outside the loaded window.
        history.load_recent(1)?;
        assert_eq!(
            FishHistoryImporter::with_path(path).import(&mut history)?,
            6
        );

        for reload in [false, true] {
            if reload {
                history.load()?;
            }
            let rows: Vec<_> = history
                .iter()
                .map(|e| (e.entry.as_str(), e.when, e.count))
                .collect();
            assert_eq!(
                rows,
                [
                    ("Hit imported", 90, 2),
                    ("Hit old", 100, 5),
                    ("Resident", 100, 1),
                    ("Hit last tie", 100, 1),
                    ("Hit first tie", 100, 2),
                ]
            );
            assert_eq!(history.search_first("Hit"), Some("Hit first tie"));
            let old = history
                .search_entries(&crate::history::HistoryQuery {
                    text: Some("HIT OLD".into()),
                    ..Default::default()
                })
                .pop()
                .unwrap();
            assert_eq!(old.context.as_deref(), Some("/project"));
            assert_eq!((old.exit_code, old.duration_ms), (Some(7), Some(123)));
            assert_eq!(old.cwd.as_deref(), Some("/work"));
            assert_eq!(old.session_id.as_deref(), Some("session"));
            assert_eq!(old.hostname.as_deref(), Some("host"));
        }
        Ok(())
    }

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
        // Create a temporary history file for dsh with a unique name to avoid conflicts during parallel testing
        let db_name = format!("dsh_test_import_{}", uuid::Uuid::new_v4());
        let mut history = History::from_file(&db_name)?;

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

        // Normal line
        writeln!(file, "- cmd: ls -la")?;
        writeln!(file, "  when: 1625097600")?;

        // Line containing invalid UTF-8 sequence
        // 0x80, 0x90 are invalid UTF-8 sequences in isolation
        file.write_all(b"- cmd: echo \x80\x90 invalid utf8\n")?;
        writeln!(file, "  when: 1625097601")?;

        // Normal line
        writeln!(file, "- cmd: echo hello")?;
        writeln!(file, "  when: 1625097602")?;

        file.flush()?;

        // Create a temporary history file for dsh
        let _dsh_history_path = temp_dir.path().join("dsh_history");
        // Create a temporary history file for dsh with a unique name to avoid conflicts during parallel testing
        let db_name = format!("dsh_test_import_{}", uuid::Uuid::new_v4());
        let mut history = History::from_file(&db_name)?;

        // Import the fish history - should succeed even with invalid UTF-8
        let importer = FishHistoryImporter::with_path(&history_path);
        let count = importer.import(&mut history)?;

        // 3 commands should be imported
        assert_eq!(count, 3);

        Ok(())
    }
}
