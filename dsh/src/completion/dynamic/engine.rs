//! The cache engine every collector funnels through: looking a value up,
//! deciding between the cached answer and a refresh under the request's
//! `CachePolicy`, loading command output, project tasks, compose services and
//! external completer results, and spawning the background refresh that keeps
//! the next lookup cheap.
use super::*;

impl DynamicCompletionProvider {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn collect_cached_value_candidates<F>(
        &self,
        command_name: &str,
        value_kind: &str,
        scope_dir: PathBuf,
        current_token: &str,
        description: &str,
        cached_only: bool,
        loader: F,
    ) -> Vec<EnhancedCandidate>
    where
        F: FnOnce() -> Result<Vec<String>> + Send + 'static,
    {
        self.collect_cached_value_candidates_with_policy(
            command_name,
            value_kind,
            scope_dir,
            current_token,
            description,
            cached_only,
            CommandQueryPolicy::LOCAL,
            loader,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn collect_cached_value_candidates_with_policy<F>(
        &self,
        command_name: &str,
        value_kind: &str,
        scope_dir: PathBuf,
        current_token: &str,
        description: &str,
        cached_only: bool,
        query_policy: CommandQueryPolicy,
        loader: F,
    ) -> Vec<EnhancedCandidate>
    where
        F: FnOnce() -> Result<Vec<String>> + Send + 'static,
    {
        let values = self.load_or_lookup_command_values(
            command_name,
            value_kind,
            scope_dir,
            cached_only,
            query_policy,
            loader,
        );

        cached_value_matches(values, current_token)
            .into_iter()
            .map(|value| EnhancedCandidate {
                text: value,
                description: Some(description.to_string()),
                candidate_type: CandidateType::Argument,
                priority: 130,
            })
            .collect()
    }

    pub(super) fn load_or_lookup_command_values<F>(
        &self,
        command_name: &str,
        value_kind: &str,
        scope_dir: PathBuf,
        cached_only: bool,
        query_policy: CommandQueryPolicy,
        loader: F,
    ) -> Vec<String>
    where
        F: FnOnce() -> Result<Vec<String>> + Send + 'static,
    {
        let kind = DynamicCommandCacheKind::CommandValue {
            command: command_name.to_string(),
            value_kind: value_kind.to_string(),
        };
        if cached_only {
            self.lookup_command_values(kind, scope_dir)
        } else {
            self.load_command_values_with_policy(kind, scope_dir, query_policy, loader)
        }
    }

    pub(super) fn collect_cached_command_candidates<F>(
        &self,
        kind: DynamicCommandCacheKind,
        scope_dir: PathBuf,
        current_token: &str,
        description: &str,
        cached_only: bool,
        loader: F,
    ) -> Vec<EnhancedCandidate>
    where
        F: FnOnce() -> Result<Vec<String>> + Send + 'static,
    {
        let values = if cached_only {
            self.lookup_command_values(kind, scope_dir)
        } else {
            self.load_command_values(kind, scope_dir, loader)
        };

        cached_value_matches(values, current_token)
            .into_iter()
            .map(|value| EnhancedCandidate {
                text: value,
                description: Some(description.to_string()),
                candidate_type: CandidateType::Argument,
                priority: 130,
            })
            .collect()
    }

    pub(crate) fn collect_probe_cached_command_candidates(
        &self,
        scope_dir: PathBuf,
        current_token: &str,
        values: Vec<String>,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitBranch,
            scope_dir,
            current_token,
            "latency probe",
            false,
            move || Ok(values),
        )
    }

    pub(super) fn load_project_tasks(&self, current_dir: &Path) -> Result<Vec<task::TaskInfo>> {
        let project_root = self.cached_project_root(current_dir);
        let runtime = self.task_discovery_runtime();
        let cache_key = TaskCacheKey {
            project_root: project_root.clone(),
            sources: Vec::new(),
        };
        let signature = task::discovery_signature(&project_root, None, &runtime);

        if let Some(tasks) = self.lookup_task_cache(&cache_key, &signature) {
            return Ok(tasks);
        }

        let tasks = task::list_tasks_in_dir(&project_root, &runtime)?;
        self.cache.write().tasks.insert(
            cache_key,
            TaskCacheEntry {
                signature,
                tasks: tasks.clone(),
            },
        );
        Ok(tasks)
    }

    pub(super) fn lookup_project_tasks(&self, current_dir: &Path) -> Vec<task::TaskInfo> {
        let project_root = self.cached_project_root(current_dir);
        let runtime = self.task_discovery_runtime();
        let cache_key = TaskCacheKey {
            project_root: project_root.clone(),
            sources: Vec::new(),
        };
        let signature = task::discovery_signature(&project_root, None, &runtime);
        self.lookup_task_cache(&cache_key, &signature)
            .unwrap_or_default()
    }

    pub(super) fn load_project_tasks_for_sources(
        &self,
        current_dir: &Path,
        sources: &[&str],
    ) -> Result<Vec<task::TaskInfo>> {
        let project_root = self.cached_project_root(current_dir);
        let runtime = self.task_discovery_runtime();
        let cache_key = TaskCacheKey {
            project_root: project_root.clone(),
            sources: normalized_task_sources(sources),
        };
        let signature = task::discovery_signature(&project_root, Some(sources), &runtime);

        if let Some(tasks) = self.lookup_task_cache(&cache_key, &signature) {
            return Ok(tasks);
        }

        let tasks = task::list_tasks_in_dir_for_sources(&project_root, sources, &runtime)?;
        self.cache.write().tasks.insert(
            cache_key,
            TaskCacheEntry {
                signature,
                tasks: tasks.clone(),
            },
        );
        Ok(tasks)
    }

    pub(super) fn lookup_project_tasks_for_sources(
        &self,
        current_dir: &Path,
        sources: &[&str],
    ) -> Vec<task::TaskInfo> {
        let project_root = self.cached_project_root(current_dir);
        let runtime = self.task_discovery_runtime();
        let cache_key = TaskCacheKey {
            project_root: project_root.clone(),
            sources: normalized_task_sources(sources),
        };
        let signature = task::discovery_signature(&project_root, Some(sources), &runtime);
        self.lookup_task_cache(&cache_key, &signature)
            .unwrap_or_default()
    }

    pub(super) fn lookup_task_cache(
        &self,
        cache_key: &TaskCacheKey,
        signature: &task::TaskDiscoverySignature,
    ) -> Option<Vec<task::TaskInfo>> {
        let cache = self.cache.read();
        let entry = cache.tasks.get(cache_key)?;
        if entry.signature == *signature {
            Some(entry.tasks.clone())
        } else {
            None
        }
    }

    fn task_discovery_runtime(&self) -> task::TaskDiscoveryRuntime {
        // One read-lock section for both halves; released before any
        // filesystem scan or provider subprocess runs.
        let env = self.environment.read();
        task::TaskDiscoveryRuntime::new(
            env.variable_state.paths.iter().map(PathBuf::from).collect(),
            env.child_process_env(),
        )
    }

    pub(super) fn load_compose_services(
        &self,
        current_dir: &Path,
        compose_file_override: Option<&Path>,
    ) -> Result<Option<(PathBuf, Vec<String>)>> {
        let compose_file = if let Some(path) = compose_file_override {
            path.to_path_buf()
        } else {
            let Some(compose_file) = find_compose_file(current_dir) else {
                return Ok(None);
            };
            compose_file
        };
        let cache_key = canonicalize_path(&compose_file);
        let signature = file_metadata_signature(&cache_key);

        if let Some(services) = self.lookup_compose_cache(&cache_key, &signature) {
            return Ok(Some((cache_key, services)));
        }

        let services = parse_compose_service_names(&cache_key)?;
        self.cache.write().compose_services.insert(
            cache_key.clone(),
            ComposeCacheEntry {
                signature,
                services: services.clone(),
            },
        );

        Ok(Some((cache_key, services)))
    }

    pub(super) fn lookup_compose_cache(
        &self,
        compose_file: &Path,
        signature: &FileMetadataSignature,
    ) -> Option<Vec<String>> {
        let cache = self.cache.read();
        let entry = cache.compose_services.get(compose_file)?;
        if entry.signature == *signature {
            Some(entry.services.clone())
        } else {
            None
        }
    }

    pub(super) fn load_command_values<F>(
        &self,
        kind: DynamicCommandCacheKind,
        scope_dir: PathBuf,
        loader: F,
    ) -> Vec<String>
    where
        F: FnOnce() -> Result<Vec<String>> + Send + 'static,
    {
        self.load_command_values_with_policy(kind, scope_dir, CommandQueryPolicy::LOCAL, loader)
    }

    pub(super) fn load_command_values_with_policy<F>(
        &self,
        kind: DynamicCommandCacheKind,
        scope_dir: PathBuf,
        query_policy: CommandQueryPolicy,
        loader: F,
    ) -> Vec<String>
    where
        F: FnOnce() -> Result<Vec<String>> + Send + 'static,
    {
        let cache_key = DynamicCommandCacheKey { kind, scope_dir };

        {
            let mut cache = self.cache.write();
            if let Some(entry) = cache.commands.get(&cache_key) {
                let values = entry.values.clone();
                let retry_allowed = cache
                    .command_errors
                    .get(&cache_key)
                    .is_none_or(|error| error.recorded_at.elapsed() >= query_policy.error_backoff);
                let start_refresh = entry.cached_at.elapsed() >= query_policy.ttl
                    && retry_allowed
                    && cache.command_pending.insert(cache_key.clone());
                update_diagnostics_from_cache(&self.runtime, &cache, None);
                drop(cache);
                if start_refresh {
                    self.mark_refresh_scheduled();
                    spawn_command_refresh(
                        self.runtime.clone(),
                        self.cache.clone(),
                        cache_key,
                        loader,
                    );
                }
                return values;
            }

            if cache
                .command_errors
                .get(&cache_key)
                .is_some_and(|error| error.recorded_at.elapsed() < query_policy.error_backoff)
            {
                update_diagnostics_from_cache(&self.runtime, &cache, None);
                return Vec::new();
            }

            if !cache.command_pending.insert(cache_key.clone()) {
                update_diagnostics_from_cache(&self.runtime, &cache, None);
                return Vec::new();
            }
            update_diagnostics_from_cache(&self.runtime, &cache, None);
        }

        self.mark_refresh_scheduled();
        spawn_command_refresh(self.runtime.clone(), self.cache.clone(), cache_key, loader);
        Vec::new()
    }

    pub(super) fn lookup_command_values(
        &self,
        kind: DynamicCommandCacheKind,
        scope_dir: PathBuf,
    ) -> Vec<String> {
        let cache_key = DynamicCommandCacheKey { kind, scope_dir };
        self.cache
            .read()
            .commands
            .get(&cache_key)
            .map(|entry| entry.values.clone())
            .unwrap_or_default()
    }

    pub(super) fn load_external_candidates<F>(
        &self,
        cache_key: ExternalCompletionCacheKey,
        loader: F,
    ) -> Result<Vec<EnhancedCandidate>>
    where
        F: FnOnce() -> Result<Vec<EnhancedCandidate>> + Send + 'static,
    {
        let ttl = Duration::from_millis(DYNAMIC_COMMAND_CACHE_TTL_MS);
        let mut start_refresh = false;

        {
            let mut cache = self.cache.write();
            if let Some(entry) = cache.external.get(&cache_key) {
                let candidates = entry.candidates.clone();
                if entry.cached_at.elapsed() >= ttl
                    && cache.external_pending.insert(cache_key.clone())
                {
                    start_refresh = true;
                }
                update_diagnostics_from_cache(&self.runtime, &cache, None);
                drop(cache);
                if start_refresh {
                    self.mark_refresh_scheduled();
                    spawn_external_refresh(
                        self.runtime.clone(),
                        self.cache.clone(),
                        cache_key,
                        loader,
                    );
                }
                return Ok(candidates);
            }

            if !cache.external_pending.insert(cache_key.clone()) {
                update_diagnostics_from_cache(&self.runtime, &cache, None);
                return Ok(Vec::new());
            }
            update_diagnostics_from_cache(
                &self.runtime,
                &cache,
                Some("external initial-load".to_string()),
            );
        }

        self.mark_refresh_scheduled();
        spawn_external_refresh(self.runtime.clone(), self.cache.clone(), cache_key, loader);
        Ok(Vec::new())
    }

    pub(super) fn resolve_command_path(&self, command_name: &str) -> Option<String> {
        self.environment.read().lookup(command_name)
    }
}
