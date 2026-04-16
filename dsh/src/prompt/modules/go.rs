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
        let source = context
            .go_source
            .map(|source| format!("({source})").dark_grey().to_string());

        if let Some(version) = &context.go_version {
            match source {
                Some(source) => Some(format!(
                    " {} {} {}",
                    "🐹".cyan().bold(),
                    version.dim(),
                    source
                )),
                None => Some(format!(" {} {}", "🐹".cyan().bold(), version.dim())),
            }
        } else if context.has_go_project {
            match source {
                Some(source) => Some(format!(" {} {}", "🐹".cyan().bold(), source)),
                None => Some(format!(" {}", "🐹".cyan().bold())),
            }
        } else {
            None
        }
    }
}
