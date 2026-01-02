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
        let cargo_toml = context.current_dir.join("Cargo.toml");

        if let Some(version) = &context.rust_version {
            Some(format!(" {} {}", "🦀".red().bold(), version.dim()))
        } else if cargo_toml.exists() {
            // Still loading or failed
            Some(format!(" {}", "🦀".red().bold()))
        } else {
            None
        }
    }
}
