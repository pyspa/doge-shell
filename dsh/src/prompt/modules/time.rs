use crate::prompt::context::PromptContext;
use crate::prompt::modules::PromptModule;
use crossterm::style::Stylize;

#[derive(Debug)]
pub struct TimeModule;

impl Default for TimeModule {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeModule {
    pub fn new() -> Self {
        Self
    }
}

impl PromptModule for TimeModule {
    fn name(&self) -> &str {
        "time"
    }

    fn render(&self, _context: &PromptContext<'_>) -> Option<String> {
        // Display current time
        let time_str = chrono::Local::now().format("%H:%M:%S").to_string();
        Some(format!(" {}", time_str.dim()))
    }
}
