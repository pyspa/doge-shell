use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;
use dsh_builtin::task;
use dsh_types::Context;

pub struct RunScriptAction;

#[async_trait(?Send)]
impl Action for RunScriptAction {
    fn name(&self) -> &'static str {
        "Run Script"
    }

    fn description(&self) -> &'static str {
        "Run a project script (npm, make, cargo, etc.)"
    }
    fn icon(&self) -> &'static str {
        "🛠️"
    }

    fn category(&self) -> &'static str {
        "Dev"
    }

    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        // Run the builtin task command interactively
        let ctx = Context::new_safe(shell.pid, shell.pgid, true);
        let argv = vec!["task".to_string()];

        let status = task::command(&ctx, argv, shell);

        match status {
            dsh_types::ExitStatus::ExitedWith(0) => Ok(()),
            _ => Err(anyhow::anyhow!("Task execution failed")),
        }
    }
}
