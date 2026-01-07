use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use std::io::{self, Write};
use std::process::Command;

pub struct GitCommitAction;

impl Action for GitCommitAction {
    fn name(&self) -> &str {
        "Git Commit"
    }
    fn description(&self) -> &str {
        "Commit staged changes"
    }
    fn category(&self) -> &str {
        "Git"
    }
    fn execute(&self, _shell: &mut Shell) -> Result<()> {
        // Check for staged changes
        let status = Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .output()?;

        if !status.status.success() {
            return Err(anyhow::anyhow!("Not in a git repository"));
        }

        let staged_files = String::from_utf8_lossy(&status.stdout);
        if staged_files.trim().is_empty() {
            println!("No staged changes. Use 'git add' first or use Git Add action.");
            return Ok(());
        }

        // Show staged files
        println!("Staged files:");
        for file in staged_files.lines() {
            println!("  {}", file);
        }
        println!();

        // Prompt for commit message
        print!("Commit message: ");
        io::stdout().flush()?;

        let mut message = String::new();
        io::stdin().read_line(&mut message)?;
        let message = message.trim();

        if message.is_empty() {
            println!("Commit cancelled (empty message)");
            return Ok(());
        }

        // Perform commit
        let result = Command::new("git")
            .args(["commit", "-m", message])
            .status()?;

        if result.success() {
            println!("Committed successfully!");
        } else {
            return Err(anyhow::anyhow!("Commit failed"));
        }

        Ok(())
    }
}
