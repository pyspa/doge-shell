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

        if package_json.exists() || node_modules.exists() {
            // TODO: Fetch node version asynchronously
            Some(format!(" {}", "⬢".green().bold()))
        } else {
            None
        }
    }
}
