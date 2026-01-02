use crate::prompt::context::PromptContext;
use crate::prompt::modules::PromptModule;
use crossterm::style::Stylize;

#[derive(Debug)]
pub struct PythonModule;

impl Default for PythonModule {
    fn default() -> Self {
        Self::new()
    }
}

impl PythonModule {
    pub fn new() -> Self {
        Self
    }
}

impl PromptModule for PythonModule {
    fn name(&self) -> &str {
        "python"
    }

    fn render(&self, context: &PromptContext<'_>) -> Option<String> {
        let has_python = context.current_dir.join("requirements.txt").exists()
            || context.current_dir.join("pyproject.toml").exists()
            || context.current_dir.join("Pipfile").exists()
            || context.current_dir.join(".venv").exists()
            || context.current_dir.join("venv").exists();

        if let Some(version) = &context.python_version {
            Some(format!(" {} {}", "🐍".yellow().bold(), version.dim()))
        } else if has_python {
            Some(format!(" {}", "🐍".yellow().bold()))
        } else {
            None
        }
    }
}
