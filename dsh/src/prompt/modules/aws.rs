use crate::prompt::context::PromptContext;
use crate::prompt::modules::PromptModule;

#[derive(Debug)]
pub struct AwsModule;

impl Default for AwsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl AwsModule {
    pub fn new() -> Self {
        Self
    }
}

impl PromptModule for AwsModule {
    fn name(&self) -> &str {
        "aws"
    }

    fn render(&self, context: &PromptContext<'_>) -> Option<String> {
        let profile = context.aws_profile.as_ref()?;
        Some(format!(" ☁️  {}", profile))
    }
}
