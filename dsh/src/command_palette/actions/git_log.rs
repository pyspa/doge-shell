use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use std::process::Command;

pub struct GitLogAction;

impl Action for GitLogAction {
    fn name(&self) -> &str {
        "Git Log"
    }
    fn description(&self) -> &str {
        "Show git log (oneline)"
    }
    fn icon(&self) -> &str {
        "📜"
    }

    fn execute(&self, _shell: &mut Shell) -> Result<()> {
        Command::new("git")
            .args(["log", "--oneline", "-20"])
            .status()
            .map_err(|e| anyhow::anyhow!("Failed to run git log: {}", e))?;
        Ok(())
    }
}
