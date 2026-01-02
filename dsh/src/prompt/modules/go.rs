use crate::prompt::context::PromptContext;
use crate::prompt::modules::PromptModule;
use crossterm::style::Stylize;

#[derive(Debug)]
pub struct GoModule;

impl Default for GoModule {
    fn default() -> Self {
        Self::new()
    }
}

impl GoModule {
    pub fn new() -> Self {
        Self
    }
}

impl PromptModule for GoModule {
    fn name(&self) -> &str {
        "go"
    }

    fn render(&self, context: &PromptContext<'_>) -> Option<String> {
        let go_mod = context.current_dir.join("go.mod");

        if let Some(version) = &context.go_version {
            Some(format!(" {} {}", "🐹".cyan().bold(), version.dim()))
        } else if go_mod.exists() {
            Some(format!(" {}", "🐹".cyan().bold()))
        } else {
            None
        }
    }
}
