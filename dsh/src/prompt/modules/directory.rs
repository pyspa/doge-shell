use crate::prompt::context::PromptContext;
use crate::prompt::modules::PromptModule;
use crossterm::style::Stylize;
use std::path::Path;

#[derive(Debug)]
pub struct DirectoryModule;

impl Default for DirectoryModule {
    fn default() -> Self {
        Self::new()
    }
}

impl DirectoryModule {
    pub fn new() -> Self {
        Self
    }
}

impl PromptModule for DirectoryModule {
    fn name(&self) -> &str {
        "directory"
    }

    fn render(&self, context: &PromptContext<'_>) -> Option<String> {
        let (path_str, is_git_context) = format_prompt_path(
            context.current_dir,
            context.git_root,
            dirs::home_dir().as_deref(),
        );

        if is_git_context {
            Some(path_str.cyan().to_string())
        } else {
            Some(path_str)
        }
    }
}

/// Helper function to format the path for the prompt
/// Returns: (formatted_path, is_git_context)
fn format_prompt_path(
    current_dir: &Path,
    git_root: Option<&Path>,
    home_dir: Option<&Path>,
) -> (String, bool) {
    if let Some(git_root) = git_root
        && current_dir.starts_with(git_root)
    {
        // Under git: show relative path from git root
        let root_name = git_root
            .file_name()
            .map_or("".to_string(), |s| s.to_string_lossy().to_string());

        let relative_path = current_dir.strip_prefix(git_root).unwrap_or(current_dir);

        let path_str = if relative_path.as_os_str().is_empty() {
            root_name
        } else {
            format!("{}/{}", root_name, relative_path.display())
        };

        return (path_str, true);
    }

    let is_git_root = current_dir.join(".git").exists();
    if is_git_root {
        let path = current_dir
            .file_name()
            .map_or("".to_owned(), |s| s.to_string_lossy().into_owned());
        (path, false)
    } else {
        let path = current_dir.display().to_string();
        if let Some(home) = home_dir {
            let home_str = home.display().to_string();
            (path.replace(&home_str, "~"), false)
        } else {
            (path, false)
        }
    }
}
