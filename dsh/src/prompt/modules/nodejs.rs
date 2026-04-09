use crate::prompt::context::PromptContext;
use crate::prompt::modules::PromptModule;
use crossterm::style::Stylize;

#[derive(Debug)]
pub struct NodeModule;

impl Default for NodeModule {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeModule {
    pub fn new() -> Self {
        Self
    }
}

impl PromptModule for NodeModule {
    fn name(&self) -> &str {
        "node"
    }

    fn render(&self, context: &PromptContext<'_>) -> Option<String> {
        let project_dir = context.project_root.unwrap_or(context.current_dir);
        let package_json = project_dir.join("package.json");
        let node_modules = project_dir.join("node_modules");
        let source = context
            .node_source
            .map(|source| format!("({source})").dark_grey().to_string());

        if let Some(version) = &context.node_version {
            match source {
                Some(source) => Some(format!(
                    " {} {} {}",
                    "⬢".green().bold(),
                    version.dim(),
                    source
                )),
                None => Some(format!(" {} {}", "⬢".green().bold(), version.dim())),
            }
        } else if package_json.exists() || node_modules.exists() {
            match source {
                Some(source) => Some(format!(" {} {}", "⬢".green().bold(), source)),
                None => Some(format!(" {}", "⬢".green().bold())),
            }
        } else {
            None
        }
    }
}
