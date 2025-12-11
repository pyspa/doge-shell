use crate::prompt::context::PromptContext;
use crate::prompt::modules::PromptModule;

#[derive(Debug)]
pub struct DockerModule;

impl Default for DockerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl DockerModule {
    pub fn new() -> Self {
        Self
    }
}

impl PromptModule for DockerModule {
    fn name(&self) -> &str {
        "docker"
    }

    fn render(&self, context: &PromptContext) -> Option<String> {
        let context_name = context.docker_context.as_ref()?;

        if context_name == "default" {
            return None;
        }

        Some(format!(" 🐳 {}", context_name))
    }
}
