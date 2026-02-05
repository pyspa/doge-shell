use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;
use std::process::Command;

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
    async fn execute(&self, _shell: &mut Shell, _input: &str) -> Result<()> {
        // Use git add -p for interactive staging
        Command::new("git")
            .args(["add", "-p"])
            .status()
            .map_err(|e| anyhow::anyhow!("Failed to run git add: {}", e))?;
        Ok(())
    }
}
