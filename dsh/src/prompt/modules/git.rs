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

    fn render(&self, context: &PromptContext<'_>) -> Option<String> {
        let Some(git_status) = &context.git_status else {
            return None;
        };

        // Branch status
        let mut status_content = String::new();

        if git_status.conflicted > 0 {
            status_content.push_str(&format!("{}{}", "=".red(), git_status.conflicted));
        }
        if git_status.ahead > 0 && git_status.behind > 0 {
            status_content.push_str(&format!(" {} ", "⇕".cyan()));
        } else {
            if git_status.ahead > 0 {
                status_content.push_str(&format!(" {}{}", "⇡".cyan(), git_status.ahead));
            }
            if git_status.behind > 0 {
                status_content.push_str(&format!(" {}{}", "⇣".cyan(), git_status.behind));
            }
        }

        if git_status.staged > 0 {
            status_content.push_str(&format!(" {}{}", "+".green(), git_status.staged));
        }
        if git_status.renamed > 0 {
            status_content.push_str(&format!(" {}{}", "»".yellow(), git_status.renamed));
        }
        if git_status.deleted > 0 {
            status_content.push_str(&format!(" {}{}", "✘".red(), git_status.deleted));
        }
        if git_status.modified > 0 {
            status_content.push_str(&format!(" {}{}", "!".yellow(), git_status.modified));
        }
        if git_status.untracked > 0 {
            status_content.push_str(&format!(" {}{}", "?".blue(), git_status.untracked));
        }

        // Branch mark and name
        // <on > <BRANCH_MARK> <branch> <status>
        let branch_display = format!(
            " {} {} {}{}",
            "on".reset(),
            self.mark.as_str().magenta(),
            git_status.branch.as_str().magenta(),
            if !status_content.is_empty() {
                format!(" [{}]", status_content.trim())
            } else {
                "".to_string()
            }
        );
        Some(branch_display)
    }
}
