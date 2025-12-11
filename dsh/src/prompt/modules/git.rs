use crate::prompt::context::PromptContext;
use crate::prompt::modules::PromptModule;
use crossterm::style::Stylize;

#[derive(Debug)]
pub struct GitModule {
    mark: String,
}

impl GitModule {
    pub fn new(mark: String) -> Self {
        Self { mark }
    }
}

impl PromptModule for GitModule {
    fn name(&self) -> &str {
        "git"
    }

    fn render(&self, context: &PromptContext) -> Option<String> {
        let Some(git_status) = &context.git_status else {
            return None;
        };

        // Ensure padding if we have prompt content
        // Write branch mark and branch name
        let branch_display = format!(
            " {} {} {}{}{}",
            "on".reset(),
            self.mark.as_str().magenta(),
            git_status.branch.as_str().magenta(),
            // Ahead/Behind counts
            if git_status.ahead > 0 || git_status.behind > 0 {
                let mut s = String::from(" ");
                if git_status.ahead > 0 {
                    s.push_str(&format!("↑{}", git_status.ahead));
                    if git_status.behind > 0 {
                        s.push(' ');
                    }
                }
                if git_status.behind > 0 {
                    s.push_str(&format!("↓{}", git_status.behind));
                }
                s.cyan().to_string()
            } else {
                "".to_string()
            },
            if let Some(status) = &git_status.branch_status {
                format!(" [{}]", status.to_string().bold().red())
            } else {
                "".to_string()
            }
        );
        Some(branch_display)
    }
}
