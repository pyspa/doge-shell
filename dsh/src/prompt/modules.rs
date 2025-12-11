use crate::prompt::context::PromptContext;

pub mod aws;
pub mod directory;
pub mod docker;
pub mod execution_time;
pub mod exit_status;
pub mod git;
pub mod go;
pub mod kubernetes;
pub mod nodejs;
pub mod python;
pub mod rust;
pub mod time;

pub trait PromptModule: Send + Sync + std::fmt::Debug {
    /// Return the name of the module (e.g., "git", "directory")
    fn name(&self) -> &str;

    /// Render the module using currently cached data.
    /// Returns None if the module should not be displayed.
    fn render(&self, context: &PromptContext) -> Option<String>;
}
