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
        let project_dir = context.project_root.unwrap_or(context.current_dir);
        let has_python = project_dir.join("requirements.txt").exists()
            || project_dir.join("pyproject.toml").exists()
            || project_dir.join("Pipfile").exists()
            || project_dir.join(".venv").exists()
            || project_dir.join("venv").exists();
        let source = context
            .python_source
            .map(|source| format!("({source})").dark_grey().to_string());

        if let Some(version) = &context.python_version {
            match source {
                Some(source) => Some(format!(
                    " {} {} {}",
                    "🐍".yellow().bold(),
                    version.dim(),
                    source
                )),
                None => Some(format!(" {} {}", "🐍".yellow().bold(), version.dim())),
            }
        } else if has_python {
            match source {
                Some(source) => Some(format!(" {} {}", "🐍".yellow().bold(), source)),
                None => Some(format!(" {}", "🐍".yellow().bold())),
            }
        } else {
            None
        }
    }
}
