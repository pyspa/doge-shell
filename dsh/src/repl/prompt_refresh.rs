use crate::environment::Environment;
use crate::prompt::{
    Prompt, PromptEnvironment, fetch_aws_profile_from, fetch_docker_context_async_from,
};
use parking_lot::RwLock;
use std::sync::Arc;

pub(crate) struct PromptRefreshCoordinator {
    prompt: Arc<RwLock<Prompt>>,
    environment: Arc<RwLock<Environment>>,
}

impl PromptRefreshCoordinator {
    pub fn new(prompt: Arc<RwLock<Prompt>>, environment: Arc<RwLock<Environment>>) -> Self {
        Self {
            prompt,
            environment,
        }
    }

    /// One snapshot per refresh tick: every probe in the tick reads the same
    /// shell values, and a shell-level unset stays unset no matter what the
    /// process environment still holds. The `Prompt` itself never owns the
    /// `Environment` (that would tangle the chpwd-hook ownership); the
    /// coordinator holds it and hands the prompt explicit input.
    fn snapshot(&self) -> PromptEnvironment {
        PromptEnvironment::from_environment(&self.environment.read())
    }

    pub fn schedule(&self) {
        self.schedule_rust();
        self.schedule_node();
        self.schedule_python();
        self.schedule_go();
        let snapshot = self.snapshot();
        self.schedule_kubernetes(&snapshot);
        self.refresh_aws(&snapshot);
        self.schedule_docker(&snapshot);
    }

    fn schedule_rust(&self) {
        if !self.prompt.read().needs_rust_check() {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        tokio::spawn(async move {
            if let Some(version) = crate::prompt::fetch_rust_version_async().await {
                prompt.write().update_rust_version(Some(version));
            } else {
                prompt.write().mark_rust_check_failed();
            }
        });
    }

    fn schedule_node(&self) {
        if !self.prompt.read().needs_node_check() {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        tokio::spawn(async move {
            if let Some(version) = crate::prompt::fetch_node_version_async().await {
                prompt.write().update_node_version(Some(version));
            } else {
                prompt.write().mark_node_check_failed();
            }
        });
    }

    fn schedule_python(&self) {
        if !self.prompt.read().needs_python_check() {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        tokio::spawn(async move {
            if let Some(version) = crate::prompt::fetch_python_version_async().await {
                prompt.write().update_python_version(Some(version));
            } else {
                prompt.write().mark_python_check_failed();
            }
        });
    }

    fn schedule_go(&self) {
        if !self.prompt.read().needs_go_check() {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        tokio::spawn(async move {
            if let Some(version) = crate::prompt::fetch_go_version_async().await {
                prompt.write().update_go_version(Some(version));
            } else {
                prompt.write().mark_go_check_failed();
            }
        });
    }

    fn schedule_kubernetes(&self, snapshot: &PromptEnvironment) {
        if !self.prompt.read().should_check_k8s(snapshot) {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        tokio::spawn(async move {
            if let Some((context, namespace)) = crate::prompt::fetch_k8s_info_async().await {
                prompt.write().update_k8s_info(Some(context), namespace);
            } else {
                prompt.write().mark_k8s_check_failed();
            }
        });
    }

    fn refresh_aws(&self, snapshot: &PromptEnvironment) {
        if self.prompt.read().should_check_aws() {
            let profile = fetch_aws_profile_from(snapshot);
            self.prompt.write().update_aws_profile(profile);
        }
    }

    fn schedule_docker(&self, snapshot: &PromptEnvironment) {
        if !self.prompt.read().should_check_docker(snapshot) {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        let snapshot = snapshot.clone();
        tokio::spawn(async move {
            if let Some(context) = fetch_docker_context_async_from(&snapshot).await {
                prompt.write().update_docker_context(Some(context));
            } else {
                prompt.write().mark_docker_check_failed();
            }
        });
    }
}
