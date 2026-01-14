use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use skim::prelude::*;
use std::process::{Command, Stdio};

pub struct GitPushPullAction;

impl Action for GitPushPullAction {
    fn name(&self) -> &str {
        "Git Push/Pull"
    }
    fn description(&self) -> &str {
        "Push or pull from remote"
    }
    fn icon(&self) -> &str {
        "⇅"
    }

    fn category(&self) -> &str {
        "Git"
    }
    fn execute(&self, _shell: &mut Shell, _input: &str) -> Result<()> {
        // Get current branch
        let branch_output = Command::new("git")
            .args(["branch", "--show-current"])
            .stdout(Stdio::piped())
            .output()?;

        let current_branch = String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string();

        if current_branch.is_empty() {
            return Err(anyhow::anyhow!("Not on a branch"));
        }

        // Select action
        let actions = vec![
            format!("push (git push origin {})", current_branch),
            format!("pull (git pull origin {})", current_branch),
            "push --force-with-lease".to_string(),
            "fetch --all".to_string(),
        ];

        let options = SkimOptionsBuilder::default()
            .prompt("Action> ".to_string())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for action in &actions {
            let _ = tx.send(Arc::new(action.clone()));
        }
        drop(tx);

        let selected = Skim::run_with(&options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(item) = selected.first() {
            let action = item.output().to_string();

            let args: Vec<&str> = if action.starts_with("push (") {
                vec!["push", "origin", &current_branch]
            } else if action.starts_with("pull (") {
                vec!["pull", "origin", &current_branch]
            } else if action.starts_with("push --force") {
                vec!["push", "--force-with-lease", "origin", &current_branch]
            } else {
                vec!["fetch", "--all"]
            };

            println!("git {}", args.join(" "));
            Command::new("git")
                .args(&args)
                .status()
                .map_err(|e| anyhow::anyhow!("Failed: {}", e))?;
        }

        Ok(())
    }
}
