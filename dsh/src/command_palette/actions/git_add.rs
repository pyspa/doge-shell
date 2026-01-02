use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use std::process::Command;

pub struct GitAddAction;

impl Action for GitAddAction {
    fn name(&self) -> &str {
        "Git Add"
    }
    fn description(&self) -> &str {
        "Interactive git add"
    }
    fn execute(&self, _shell: &mut Shell) -> Result<()> {
        // Use git add -p for interactive staging
        Command::new("git")
            .args(["add", "-p"])
            .status()
            .map_err(|e| anyhow::anyhow!("Failed to run git add: {}", e))?;
        Ok(())
    }
}
