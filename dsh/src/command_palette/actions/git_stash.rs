use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use skim::prelude::*;
use std::process::{Command, Stdio};

pub struct GitStashAction;

impl Action for GitStashAction {
    fn name(&self) -> &str {
        "Git Stash"
    }
    fn description(&self) -> &str {
        "Manage git stash entries"
    }
    fn icon(&self) -> &str {
        "📦"
    }

    fn execute(&self, _shell: &mut Shell, _input: &str) -> Result<()> {
        // Get stash list
        let output = Command::new("git")
            .args(["stash", "list"])
            .stdout(Stdio::piped())
            .output()?;

        if !output.status.success() {
            return Err(anyhow::anyhow!("Failed to list stash entries"));
        }

        let stashes = String::from_utf8_lossy(&output.stdout);
        let stash_list: Vec<&str> = stashes.lines().collect();

        if stash_list.is_empty() {
            println!("No stash entries");
            return Ok(());
        }

        // First, select a stash entry
        let stash_options = SkimOptionsBuilder::default()
            .prompt(Some("Stash Action> "))
            .bind(vec!["Enter:accept", "Esc:abort"])
            .preview(Some("git stash show -p {}"))
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for stash in &stash_list {
            let _ = tx.send(Arc::new(stash.to_string()));
        }
        drop(tx);

        let selected = Skim::run_with(&stash_options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if selected.is_empty() {
            return Ok(());
        }

        let stash_entry = selected[0].output().to_string();
        // Extract stash ref (e.g., "stash@{0}")
        let stash_ref = stash_entry
            .split(':')
            .next()
            .unwrap_or("stash@{0}")
            .to_string();

        // Then, select an action
        let actions = vec!["apply", "pop", "drop", "show"];
        let action_options = SkimOptionsBuilder::default()
            .prompt(Some("Action> "))
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for action in actions {
            let _ = tx.send(Arc::new(action.to_string()));
        }
        drop(tx);

        let selected_action = Skim::run_with(&action_options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(action_item) = selected_action.first() {
            let action = action_item.output().to_string();

            println!("git stash {} {}", action, stash_ref);
            Command::new("git")
                .args(["stash", &action, &stash_ref])
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to execute stash {}: {}", action, e))?;
        }

        Ok(())
    }
}
