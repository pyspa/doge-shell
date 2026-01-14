use crate::command_palette::Action;
use crate::shell::Shell;
use anyhow::Result;

/// An action that executes a Lisp function.
///
/// Function name is stored as a String to ensure thread-safety (Send + Sync).
#[derive(Debug, Clone)]
pub struct LispAction {
    pub name: String,
    pub description: String,
    pub function_name: String,
}

impl Action for LispAction {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        let engine = shell.lisp_engine.borrow();
        engine.run_func_values(&self.function_name, vec![])?;
        Ok(())
    }
}
