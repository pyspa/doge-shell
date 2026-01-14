use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use skim::prelude::*;
use std::process::{Command, Stdio};

pub struct GitDiffAction;

impl Action for GitDiffAction {
    fn name(&self) -> &str {
        "Git Diff"
    }
    fn description(&self) -> &str {
        "Show diff for changed files"
    }
    fn icon(&self) -> &str {
        "📝"
    }

    fn category(&self) -> &str {
        "Git"
    }
    fn execute(&self, _shell: &mut Shell, _input: &str) -> Result<()> {
        // Get list of changed files
        let output = Command::new("git")
            .args(["diff", "--name-only"])
            .stdout(Stdio::piped())
            .output()?;

        if !output.status.success() {
            return Err(anyhow::anyhow!("Failed to get changed files"));
        }

        let files = String::from_utf8_lossy(&output.stdout);
        let mut file_list: Vec<String> = files.lines().map(|s| s.to_string()).collect();

        // Also include staged files
        let staged_output = Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .stdout(Stdio::piped())
            .output()?;

        if staged_output.status.success() {
            let staged_files = String::from_utf8_lossy(&staged_output.stdout);
            for file in staged_files.lines() {
                let file_str = file.to_string();
                if !file_list.contains(&file_str) {
                    file_list.push(file_str);
                }
            }
        }

        if file_list.is_empty() {
            println!("No changed files");
            return Ok(());
        }

        // Show selection UI
        let options = SkimOptionsBuilder::default()
            .prompt("File> ".to_string())
            .preview(Some("git diff --color=always -- {}".to_string()))
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for file in file_list {
            let _ = tx.send(Arc::new(file));
        }
        drop(tx);

        let selected = Skim::run_with(&options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(item) = selected.first() {
            let file_path = item.output().to_string();

            // Show full diff for selected file
            Command::new("git")
                .args(["diff", "--color=always", "--", &file_path])
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to show diff: {}", e))?;
        }

        Ok(())
    }
}
