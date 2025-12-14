use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;

pub struct ReloadConfigAction;
impl Action for ReloadConfigAction {
    fn name(&self) -> &str {
        "Reload Config"
    }
    fn description(&self) -> &str {
        "Reload config.lisp"
    }
    fn execute(&self, shell: &mut Shell) -> Result<()> {
        shell.lisp_engine.borrow().run_config_lisp()?;
        Ok(())
    }
}
