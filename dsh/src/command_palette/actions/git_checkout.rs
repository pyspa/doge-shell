use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use skim::prelude::*;
use std::process::{Command, Stdio};

pub struct GitCheckoutAction;

impl Action for GitCheckoutAction {
    fn name(&self) -> &str {
        "Git Checkout"
    }
    fn description(&self) -> &str {
        "Interactive git checkout branch"
    }
    fn icon(&self) -> &str {
        "🌿"
    }

    fn execute(&self, _shell: &mut Shell, _input: &str) -> Result<()> {
        // Get branches
        let output = Command::new("git")
            .args(["branch", "-a", "--format=%(refname:short)"])
            .stdout(Stdio::piped())
            .output()?;

        if !output.status.success() {
            return Err(anyhow::anyhow!("Failed to list branches"));
        }

        let branches = String::from_utf8_lossy(&output.stdout);
        let branch_list: Vec<&str> = branches.lines().collect();

        if branch_list.is_empty() {
            return Err(anyhow::anyhow!("No branches found"));
        }

        // Show selection UI
        let options = SkimOptionsBuilder::default()
            .prompt("Branch> ".to_string())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for branch in branch_list {
            let _ = tx.send(Arc::new(branch.to_string()));
        }
        drop(tx);

        let selected = Skim::run_with(&options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(item) = selected.first() {
            let branch = item.output().to_string();
            Command::new("git")
                .args(["checkout", &branch])
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to checkout: {}", e))?;
        }

        Ok(())
    }
}
