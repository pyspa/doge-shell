use crate::prompt::context::PromptContext;
use crate::prompt::modules::PromptModule;
use crossterm::style::Stylize;

#[derive(Debug)]
pub struct ExitStatusModule;

impl Default for ExitStatusModule {
    fn default() -> Self {
        Self::new()
    }
}

impl ExitStatusModule {
    pub fn new() -> Self {
        Self
    }
}

impl PromptModule for ExitStatusModule {
    fn name(&self) -> &str {
        "exit_status"
    }

    fn render(&self, context: &PromptContext<'_>) -> Option<String> {
        if context.last_exit_status != 0 {
            Some(format!(
                " {} {}",
                "✘".red().bold(),
                context.last_exit_status.to_string().red()
            ))
        } else {
            None
        }
    }
}
