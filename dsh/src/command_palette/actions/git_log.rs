use super::super::Action;
use crate::shell::Shell;
use anyhow::{Context as _, Result};
use async_trait::async_trait;

pub struct GitLogAction;

#[async_trait(?Send)]
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

    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        let runtime = super::runtime_snapshot(shell);
        runtime
            .std_command("git")
            .context("command not found: git")?
            .args(["log", "--oneline", "-20"])
            .status()
            .map_err(|e| anyhow::anyhow!("Failed to run git log: {}", e))?;
        Ok(())
    }
}
