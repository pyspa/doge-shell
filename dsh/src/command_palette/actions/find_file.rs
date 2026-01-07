use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use skim::prelude::*;
use std::process::{Command, Stdio};

pub struct FindFileAction;

impl Action for FindFileAction {
    fn name(&self) -> &str {
        "Find File"
    }
    fn description(&self) -> &str {
        "Search and open file in $EDITOR"
    }
    fn execute(&self, _shell: &mut Shell) -> Result<()> {
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

        // Show selection UI
        let options = SkimOptionsBuilder::default()
            .prompt("File> ".to_string())
            .preview(Some("head -50 {}".to_string()))
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for file in file_list {
            let _ = tx.send(Arc::new(file.to_string()));
        }
        drop(tx);

        let selected = Skim::run_with(&options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(item) = selected.first() {
            let file_path = item.output().to_string();

            // Get editor from environment
            let editor = std::env::var("EDITOR")
                .or_else(|_| std::env::var("VISUAL"))
                .unwrap_or_else(|_| "vim".to_string());

            Command::new(&editor)
                .arg(&file_path)
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to open editor: {}", e))?;
        }

        Ok(())
    }
}
