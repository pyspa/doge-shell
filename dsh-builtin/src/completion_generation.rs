use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Shared boundary for completion generation used by both `dsh completion`
/// and the `comp-gen` builtin.
pub struct CompletionGenerationService;

impl CompletionGenerationService {
    pub fn validate_command_name(command_name: &str) -> Result<()> {
        let mut chars = command_name.chars();
        let Some(first) = chars.next() else {
            bail!("Command name must not be empty");
        };
        if !(first.is_ascii_alphanumeric() || first == '_') {
            bail!("Command name must start with an ASCII letter, digit, or underscore");
        }
        if !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '+')) {
            bail!("Command name may only contain ASCII letters, digits, '_', '-', '.', and '+'");
        }
        if Path::new(command_name)
            .file_name()
            .and_then(|name| name.to_str())
            != Some(command_name)
        {
            bail!("Command name must be a basename without path separators");
        }
        Ok(())
    }

    pub fn collect_help_text(command_name: &str) -> Result<String> {
        Self::validate_command_name(command_name)?;

        let man_output = Command::new("man")
            .arg("-P")
            .arg("cat")
            .arg(command_name)
            .output();
        if let Ok(output) = man_output
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            if !stdout.trim().is_empty() {
                return Ok(stdout);
            }
        }

        let output = Command::new(command_name)
            .arg("--help")
            .output()
            .with_context(|| format!("Failed to execute '{command_name} --help'"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let help_text = if stdout.trim().is_empty() {
            stderr
        } else {
            stdout
        };
        if help_text.trim().is_empty() {
            bail!(
                "Could not retrieve help text for '{command_name}'. Ensure the command is installed and supports --help"
            );
        }
        Ok(help_text)
    }

    pub fn validate_json(json: &str, expected_command: &str) -> Result<Value> {
        Self::validate_command_name(expected_command)?;
        crate::comp_gen::validate_completion_json(json, expected_command)?;
        serde_json::from_str(json).context("Completion output is not valid JSON")
    }

    pub fn default_output_path(command_name: &str) -> Result<PathBuf> {
        Self::validate_command_name(command_name)?;
        let xdg_dirs = xdg::BaseDirectories::with_prefix("dsh");
        xdg_dirs
            .place_config_file(format!("completions/{command_name}.json"))
            .context("Failed to resolve the completion output path")
    }

    pub fn write_json_atomic(
        output_path: &Path,
        json: &str,
        expected_command: &str,
        force: bool,
    ) -> Result<()> {
        let value = Self::validate_json(json, expected_command)?;
        let formatted = serde_json::to_string_pretty(&value)
            .context("Failed to format completion JSON for writing")?;
        let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create completion directory '{}'",
                parent.display()
            )
        })?;

        let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
            format!(
                "Failed to create a temporary completion file in '{}'",
                parent.display()
            )
        })?;
        temporary
            .write_all(formatted.as_bytes())
            .context("Failed to write temporary completion file")?;
        temporary
            .as_file()
            .sync_all()
            .context("Failed to flush temporary completion file")?;

        if force {
            temporary
                .persist(output_path)
                .map_err(|error| error.error)?;
        } else {
            temporary
                .persist_noclobber(output_path)
                .map_err(|error| error.error)
                .with_context(|| {
                    format!(
                        "Completion file already exists: {}. Use --force to overwrite",
                        output_path.display()
                    )
                })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::CompletionGenerationService;

    #[test]
    fn command_name_rejects_shell_and_path_syntax() {
        for name in ["", "../git", "/bin/git", "git;echo", "git branch", "$(git)"] {
            assert!(
                CompletionGenerationService::validate_command_name(name).is_err(),
                "{name:?} must be rejected"
            );
        }
        for name in ["git", "cargo-nextest", "python3.13", "c++"] {
            assert!(
                CompletionGenerationService::validate_command_name(name).is_ok(),
                "{name:?} must be accepted"
            );
        }
    }

    #[test]
    fn atomic_write_preserves_existing_file_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("foo.json");
        let first = r#"{"command":"foo","description":"first"}"#;
        let second = r#"{"command":"foo","description":"second"}"#;

        CompletionGenerationService::write_json_atomic(&output, first, "foo", false).unwrap();
        assert!(
            CompletionGenerationService::write_json_atomic(&output, second, "foo", false).is_err()
        );
        assert!(fs::read_to_string(&output).unwrap().contains("first"));

        CompletionGenerationService::write_json_atomic(&output, second, "foo", true).unwrap();
        assert!(fs::read_to_string(&output).unwrap().contains("second"));
    }

    use std::fs;
}
