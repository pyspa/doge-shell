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

    fn render(&self, context: &PromptContext) -> Option<String> {
        let package_json = context.current_dir.join("package.json");
        let node_modules = context.current_dir.join("node_modules");

        if let Some(version) = &context.node_version {
            Some(format!(
                " {} {}",
                "⬢".green().bold(),
                version.as_str().dim()
            ))
        } else if package_json.exists() || node_modules.exists() {
            // Still loading
            Some(format!(" {}", "⬢".green().bold()))
        } else {
            None
        }
    }
}
