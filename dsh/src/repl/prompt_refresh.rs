use crate::environment::Environment;
use crate::prompt::{
    Prompt, PromptRuntimeSnapshot, fetch_aws_profile_from, fetch_docker_context_async_from,
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
    ///
    /// The `Prompt` read lock is released before the `Environment` lock is
    /// taken so the chpwd hook lock order is never inverted.
    fn snapshot(&self) -> PromptRuntimeSnapshot {
        let current_dir = self.prompt.read().current_path().to_path_buf();
        PromptRuntimeSnapshot::from_environment(&self.environment.read(), current_dir)
    }

    pub fn schedule(&self) {
        // A single runtime for the whole tick, shared by every probe below.
        let runtime = Arc::new(self.snapshot());
        // Observe before any `needs_*_check`: a PATH generation change must
        // invalidate stale caches before the checks read them.
        self.prompt
            .write()
            .observe_runtime_path_generation(runtime.path_generation());

        self.schedule_rust(&runtime);
        self.schedule_node(&runtime);
        self.schedule_python(&runtime);
        self.schedule_go(&runtime);

        self.schedule_kubernetes(&runtime);
        self.refresh_aws(&runtime);
        self.schedule_docker(&runtime);
    }

    fn schedule_rust(&self, runtime: &Arc<PromptRuntimeSnapshot>) {
        if !self.prompt.read().needs_rust_check() {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        let runtime = Arc::clone(runtime);
        tokio::spawn(async move {
            if let Some(version) = crate::prompt::fetch_rust_version_async(&runtime).await {
                prompt.write().update_rust_version(Some(version));
            } else {
                prompt.write().mark_rust_check_failed();
            }
        });
    }

    fn schedule_node(&self, runtime: &Arc<PromptRuntimeSnapshot>) {
        if !self.prompt.read().needs_node_check() {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        let runtime = Arc::clone(runtime);
        tokio::spawn(async move {
            if let Some(version) = crate::prompt::fetch_node_version_async(&runtime).await {
                prompt.write().update_node_version(Some(version));
            } else {
                prompt.write().mark_node_check_failed();
            }
        });
    }

    fn schedule_python(&self, runtime: &Arc<PromptRuntimeSnapshot>) {
        if !self.prompt.read().needs_python_check() {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        let runtime = Arc::clone(runtime);
        tokio::spawn(async move {
            if let Some(version) = crate::prompt::fetch_python_version_async(&runtime).await {
                prompt.write().update_python_version(Some(version));
            } else {
                prompt.write().mark_python_check_failed();
            }
        });
    }

    fn schedule_go(&self, runtime: &Arc<PromptRuntimeSnapshot>) {
        if !self.prompt.read().needs_go_check() {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        let runtime = Arc::clone(runtime);
        tokio::spawn(async move {
            if let Some(version) = crate::prompt::fetch_go_version_async(&runtime).await {
                prompt.write().update_go_version(Some(version));
            } else {
                prompt.write().mark_go_check_failed();
            }
        });
    }

    fn schedule_kubernetes(&self, runtime: &Arc<PromptRuntimeSnapshot>) {
        if !self.prompt.read().should_check_k8s(runtime) {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        let runtime = Arc::clone(runtime);
        tokio::spawn(async move {
            if let Some((context, namespace)) = crate::prompt::fetch_k8s_info_async(&runtime).await
            {
                prompt.write().update_k8s_info(Some(context), namespace);
            } else {
                prompt.write().mark_k8s_check_failed();
            }
        });
    }

    fn refresh_aws(&self, runtime: &PromptRuntimeSnapshot) {
        if self.prompt.read().should_check_aws() {
            let profile = fetch_aws_profile_from(&runtime.environment);
            self.prompt.write().update_aws_profile(profile);
        }
    }

    fn schedule_docker(&self, runtime: &Arc<PromptRuntimeSnapshot>) {
        if !self.prompt.read().should_check_docker(runtime) {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        let runtime = Arc::clone(runtime);
        tokio::spawn(async move {
            if let Some(context) = fetch_docker_context_async_from(&runtime).await {
                prompt.write().update_docker_context(Some(context));
            } else {
                prompt.write().mark_docker_check_failed();
            }
        });
    }
}
