use crate::prompt::context::PromptContext;
use crate::prompt::modules::PromptModule;
// use crossterm::style::Stylize; // Use if we need colors

#[derive(Debug)]
pub struct KubernetesModule;

impl Default for KubernetesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl KubernetesModule {
    pub fn new() -> Self {
        Self
    }
}

impl PromptModule for KubernetesModule {
    fn name(&self) -> &str {
        "kubernetes"
    }

    fn render(&self, context: &PromptContext) -> Option<String> {
        let k8s_context = context.k8s_context.as_ref()?;
        let namespace = context.k8s_namespace.as_deref();

        let mut output = String::from(" ☸️ ");
        output.push_str(k8s_context);

        if let Some(ns) = namespace {
            output.push_str(" (");
            output.push_str(ns);
            output.push(')');
        }

        Some(output)
    }
}
