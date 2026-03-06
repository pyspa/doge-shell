use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;

pub struct ReloadConfigAction;
#[async_trait(?Send)]
impl Action for ReloadConfigAction {
    fn name(&self) -> &str {
        "Reload Config"
    }
    fn description(&self) -> &str {
        "Reload config.lisp"
    }
    fn icon(&self) -> &str {
        "🔄"
    }

    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        if let Err(e) = shell.lisp_engine.borrow().run_config_lisp() {
            eprintln!("Warning: Failed to load config.lisp: {}", e);
            return Err(e);
        }
        shell.reload_mcp_config();
        Ok(())
    }
}
