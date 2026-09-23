use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;
use skim::prelude::*;
use std::process::{Command, Stdio};

pub struct FindFileAction;

/// Which editor the find-file action opens.
///
/// Resolution order: the `EDITOR` shell variable, then `VISUAL`, then `vim`.
/// A blank value counts as unset. A shell-level miss stays a miss even when
/// the process environment holds a stale value: the shell `Environment` is
/// the runtime authority.
fn resolve_editor(shell: &Shell) -> String {
    let environment = shell.environment.read();
    resolve_editor_from(&environment)
}

/// The `EDITOR` / `VISUAL` lookup behind [`resolve_editor`], split out so
/// tests can assert the precedence without building a `Shell`.
fn resolve_editor_from(environment: &crate::environment::Environment) -> String {
    for key in ["EDITOR", "VISUAL"] {
        if let Some(value) = environment
            .get_var(key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            return value;
        }
    }
    "vim".to_string()
}

#[async_trait(?Send)]
impl Action for FindFileAction {
    fn name(&self) -> &str {
        "Find File"
    }
    fn description(&self) -> &str {
        "Search and open file in $EDITOR"
    }
    fn icon(&self) -> &str {
        "🔍"
    }

    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        // Try fd first, fall back to find
        let output = Command::new("fd")
            .args(["--type", "f", "--hidden", "--exclude", ".git"])
            .stdout(Stdio::piped())
            .output()
            .or_else(|_| {
                Command::new("find")
                    .args([".", "-type", "f", "-not", "-path", "*/.git/*"])
                    .stdout(Stdio::piped())
                    .output()
            })?;

        if !output.status.success() {
            return Err(anyhow::anyhow!("Failed to list files"));
        }

        let files = String::from_utf8_lossy(&output.stdout);
        let file_list: Vec<&str> = files.lines().collect();

        if file_list.is_empty() {
            println!("No files found");
            return Ok(());
        }

        use crate::command_palette::StringItem;

        // Show selection UI
        let options = SkimOptionsBuilder::default()
            .prompt("File> ".to_string())
            .preview("head -50 {}".to_string())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for file in file_list {
            let _ = tx.send(vec![Arc::new(StringItem(file.to_string()))]);
        }
        drop(tx);

        let selected = crate::utils::skim::run_skim_with(options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(item) = selected.first() {
            let file_path = item.output().to_string();

            // Get editor from environment
            let editor = resolve_editor(shell);

            Command::new(&editor)
                .arg(&file_path)
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to open editor: {}", e))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProcessEnvGuard;
    use crate::environment::Environment;

    /// A stale process-only `EDITOR` must not resurrect: with no shell
    /// value, the action falls back to `vim`.
    #[test]
    fn process_only_editor_is_ignored() {
        let _lock = crate::test_env_lock();
        let _guard = ProcessEnvGuard::set("EDITOR", "stale-editor");
        let environment = Environment::new();
        environment.write().unset_shell_var("EDITOR");
        environment.write().unset_shell_var("VISUAL");
        let shell = Shell::new(environment);

        assert_eq!(resolve_editor(&shell), "vim");
    }

    /// A shell `EDITOR` wins over the stale process value.
    #[test]
    fn shell_editor_wins_over_process() {
        let _lock = crate::test_env_lock();
        let _guard = ProcessEnvGuard::set("EDITOR", "stale-editor");
        let environment = Environment::new();
        environment
            .write()
            .set_shell_var("EDITOR".to_string(), "nvim".to_string());
        let shell = Shell::new(environment);

        assert_eq!(resolve_editor(&shell), "nvim");
    }

    /// With no shell `EDITOR`, a shell `VISUAL` is used.
    #[test]
    fn shell_visual_is_the_fallback() {
        let _lock = crate::test_env_lock();
        let environment = Environment::new();
        environment.write().unset_shell_var("EDITOR");
        environment
            .write()
            .set_shell_var("VISUAL".to_string(), "emacs".to_string());
        let shell = Shell::new(environment);

        assert_eq!(resolve_editor(&shell), "emacs");
    }

    /// A blank shell `EDITOR` counts as unset and falls through to `VISUAL`.
    #[test]
    fn blank_shell_editor_falls_through_to_visual() {
        let _lock = crate::test_env_lock();
        let environment = Environment::new();
        environment
            .write()
            .set_shell_var("EDITOR".to_string(), "   ".to_string());
        environment
            .write()
            .set_shell_var("VISUAL".to_string(), "emacs".to_string());
        let shell = Shell::new(environment);

        assert_eq!(resolve_editor(&shell), "emacs");
    }
}
