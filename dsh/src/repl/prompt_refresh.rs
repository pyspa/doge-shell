use crate::environment::Environment;
use crate::prompt::{
    Prompt, PromptProbe, PromptProbeEpoch, PromptRuntimeSnapshot, fetch_aws_profile_from,
    fetch_docker_context_async_from,
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
        let identity = runtime.identity();
        // Observe before any `needs_*_check`: an identity change must
        // invalidate stale caches and advance the epoch before the checks
        // read them.
        let epoch = self.prompt.write().observe_runtime_identity(identity);

        self.schedule_rust(&runtime, epoch);
        self.schedule_node(&runtime, epoch);
        self.schedule_python(&runtime, epoch);
        self.schedule_go(&runtime, epoch);

        self.schedule_kubernetes(&runtime, epoch);
        self.refresh_aws(&runtime);
        self.schedule_docker(&runtime, epoch);
    }

    /// Shared publication discipline: only the completion that still owns
    /// the current epoch may mutate Prompt caches or failure backoff.
    /// Stale results are dropped (debug-logged); old tasks keep running
    /// but never roll back a newer runtime.
    fn publish_probe_result<F>(
        prompt: &Arc<RwLock<Prompt>>,
        probe: PromptProbe,
        epoch: PromptProbeEpoch,
        publish: F,
    ) where
        F: FnOnce(&mut Prompt),
    {
        let mut prompt = prompt.write();
        if !prompt.finish_probe(probe, epoch) {
            tracing::debug!("prompt probe result dropped as stale");
            return;
        }
        publish(&mut prompt);
    }

    fn schedule_rust(&self, runtime: &Arc<PromptRuntimeSnapshot>, epoch: PromptProbeEpoch) {
        let should_spawn = {
            let mut prompt = self.prompt.write();
            prompt.needs_rust_check() && prompt.try_begin_probe(PromptProbe::Rust, epoch)
        };
        if !should_spawn {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        let runtime = Arc::clone(runtime);
        tokio::spawn(async move {
            let result = crate::prompt::fetch_rust_version_async(&runtime).await;
            Self::publish_probe_result(&prompt, PromptProbe::Rust, epoch, |prompt| match result {
                Some(version) => {
                    prompt.update_rust_version(Some(version));
                }
                None => {
                    prompt.mark_rust_check_failed();
                }
            });
        });
    }

    fn schedule_node(&self, runtime: &Arc<PromptRuntimeSnapshot>, epoch: PromptProbeEpoch) {
        let should_spawn = {
            let mut prompt = self.prompt.write();
            prompt.needs_node_check() && prompt.try_begin_probe(PromptProbe::Node, epoch)
        };
        if !should_spawn {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        let runtime = Arc::clone(runtime);
        tokio::spawn(async move {
            let result = crate::prompt::fetch_node_version_async(&runtime).await;
            Self::publish_probe_result(&prompt, PromptProbe::Node, epoch, |prompt| match result {
                Some(version) => {
                    prompt.update_node_version(Some(version));
                }
                None => {
                    prompt.mark_node_check_failed();
                }
            });
        });
    }

    fn schedule_python(&self, runtime: &Arc<PromptRuntimeSnapshot>, epoch: PromptProbeEpoch) {
        let should_spawn = {
            let mut prompt = self.prompt.write();
            prompt.needs_python_check() && prompt.try_begin_probe(PromptProbe::Python, epoch)
        };
        if !should_spawn {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        let runtime = Arc::clone(runtime);
        tokio::spawn(async move {
            let result = crate::prompt::fetch_python_version_async(&runtime).await;
            Self::publish_probe_result(
                &prompt,
                PromptProbe::Python,
                epoch,
                |prompt| match result {
                    Some(version) => {
                        prompt.update_python_version(Some(version));
                    }
                    None => {
                        prompt.mark_python_check_failed();
                    }
                },
            );
        });
    }

    fn schedule_go(&self, runtime: &Arc<PromptRuntimeSnapshot>, epoch: PromptProbeEpoch) {
        let should_spawn = {
            let mut prompt = self.prompt.write();
            prompt.needs_go_check() && prompt.try_begin_probe(PromptProbe::Go, epoch)
        };
        if !should_spawn {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        let runtime = Arc::clone(runtime);
        tokio::spawn(async move {
            let result = crate::prompt::fetch_go_version_async(&runtime).await;
            Self::publish_probe_result(&prompt, PromptProbe::Go, epoch, |prompt| match result {
                Some(version) => {
                    prompt.update_go_version(Some(version));
                }
                None => {
                    prompt.mark_go_check_failed();
                }
            });
        });
    }

    fn schedule_kubernetes(&self, runtime: &Arc<PromptRuntimeSnapshot>, epoch: PromptProbeEpoch) {
        let should_spawn = {
            let mut prompt = self.prompt.write();
            prompt.should_check_k8s(runtime)
                && prompt.try_begin_probe(PromptProbe::Kubernetes, epoch)
        };
        if !should_spawn {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        let runtime = Arc::clone(runtime);
        tokio::spawn(async move {
            let result = crate::prompt::fetch_k8s_info_async(&runtime).await;
            Self::publish_probe_result(&prompt, PromptProbe::Kubernetes, epoch, |prompt| {
                match result {
                    Some((context, namespace)) => {
                        prompt.update_k8s_info(Some(context), namespace);
                    }
                    None => {
                        prompt.mark_k8s_check_failed();
                    }
                }
            });
        });
    }

    fn refresh_aws(&self, runtime: &PromptRuntimeSnapshot) {
        if self.prompt.read().should_check_aws() {
            let profile = fetch_aws_profile_from(&runtime.environment);
            self.prompt.write().update_aws_profile(profile);
        }
    }

    fn schedule_docker(&self, runtime: &Arc<PromptRuntimeSnapshot>, epoch: PromptProbeEpoch) {
        let should_spawn = {
            let mut prompt = self.prompt.write();
            prompt.should_check_docker(runtime)
                && prompt.try_begin_probe(PromptProbe::Docker, epoch)
        };
        if !should_spawn {
            return;
        }
        let prompt = Arc::clone(&self.prompt);
        let runtime = Arc::clone(runtime);
        tokio::spawn(async move {
            let result = fetch_docker_context_async_from(&runtime).await;
            Self::publish_probe_result(
                &prompt,
                PromptProbe::Docker,
                epoch,
                |prompt| match result {
                    Some(context) => {
                        prompt.update_docker_context(Some(context));
                    }
                    None => {
                        prompt.mark_docker_check_failed();
                    }
                },
            );
        });
    }
}
