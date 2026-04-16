use crate::prompt::context::PromptContext;
use crate::prompt::modules::PromptModule;
use crossterm::style::Stylize;

#[derive(Debug)]
pub struct RustModule;

impl Default for RustModule {
    fn default() -> Self {
        Self::new()
    }
}

impl RustModule {
    pub fn new() -> Self {
        Self
    }
}

impl PromptModule for RustModule {
    fn name(&self) -> &str {
        "rust"
    }

    fn render(&self, context: &PromptContext<'_>) -> Option<String> {
        let source = context
            .rust_source
            .map(|source| format!("({source})").dark_grey().to_string());

        if let Some(version) = &context.rust_version {
            match source {
                Some(source) => Some(format!(
                    " {} {} {}",
                    "🦀".red().bold(),
                    version.dim(),
                    source
                )),
                None => Some(format!(" {} {}", "🦀".red().bold(), version.dim())),
            }
        } else if context.has_rust_project {
            match source {
                Some(source) => Some(format!(" {} {}", "🦀".red().bold(), source)),
                None => Some(format!(" {}", "🦀".red().bold())),
            }
        } else {
            None
        }
    }
}
