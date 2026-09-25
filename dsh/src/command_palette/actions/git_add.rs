use super::super::Action;
use crate::shell::Shell;
use anyhow::{Context as _, Result};
use async_trait::async_trait;

pub struct GitAddAction;

#[async_trait(?Send)]
impl Action for GitAddAction {
    fn name(&self) -> &str {
        "Git Add"
    }
    fn description(&self) -> &str {
        "Interactive git add"
    }
    fn icon(&self) -> &str {
        "➕"
    }

    fn category(&self) -> &str {
        "Git"
    }
    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        let runtime = super::runtime_snapshot(shell);
        // Use git add -p for interactive staging
        runtime
            .std_command("git")
            .context("command not found: git")?
            .args(["add", "-p"])
            .status()
            .map_err(|e| anyhow::anyhow!("Failed to run git add: {}", e))?;
        Ok(())
    }
}
