use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;

/// An action that executes a Lisp function.
///
/// Function name is stored as a String to ensure thread-safety (Send + Sync).
#[derive(Debug, Clone)]
pub struct LispAction {
    pub name: String,
    pub description: String,
    pub function_name: String,
}

#[async_trait(?Send)]
impl Action for LispAction {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        let engine = shell.lisp_engine.borrow();
        engine.run_func_values(&self.function_name, vec![])?;
        Ok(())
    }
}
