use crate::prompt::context::PromptContext;
use crate::prompt::modules::PromptModule;
use crossterm::style::Stylize;

#[derive(Debug)]
pub struct ExecutionTimeModule;

impl Default for ExecutionTimeModule {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionTimeModule {
    pub fn new() -> Self {
        Self
    }
}

impl PromptModule for ExecutionTimeModule {
    fn name(&self) -> &str {
        "execution_time"
    }

    fn render(&self, context: &PromptContext) -> Option<String> {
        let duration = context.last_duration?;
        if duration.as_secs() >= 2 {
            let secs = duration.as_secs();
            let time_str = if secs < 60 {
                format!("{}s", secs)
            } else {
                format!("{}m{}s", secs / 60, secs % 60)
            };

            Some(format!(" {}", time_str.yellow()))
        } else {
            None
        }
    }
}
