use crate::prompt::Prompt;
use parking_lot::RwLock;
use std::sync::Arc;

pub(crate) struct PromptRefreshCoordinator {
    prompt: Arc<RwLock<Prompt>>,
}

impl PromptRefreshCoordinator {
    pub fn new(prompt: Arc<RwLock<Prompt>>) -> Self {
        Self { prompt }
    }

    pub fn schedule(&self) {
        self.schedule_rust();
        self.schedule_node();
        self.schedule_python();
        self.schedule_go();
        self.schedule_kubernetes();
        self.refresh_aws();
        self.schedule_docker();
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

    fn schedule_kubernetes(&self) {
        if !self.prompt.read().should_check_k8s() {
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

    fn refresh_aws(&self) {
        if self.prompt.read().should_check_aws() {
            let profile = crate::prompt::fetch_aws_profile();
            self.prompt.write().update_aws_profile(profile);
        }
    }

    fn schedule_docker(&self) {
        if !self.prompt.read().should_check_docker() {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        tokio::spawn(async move {
            if let Some(context) = crate::prompt::fetch_docker_context_async().await {
                prompt.write().update_docker_context(Some(context));
            } else {
                prompt.write().mark_docker_check_failed();
            }
        });
    }
}
