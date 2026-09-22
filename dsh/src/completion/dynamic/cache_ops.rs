//! Maintaining the per-project dynamic cache around the collectors: spawning a
//! background refresh, pruning expired command/error/external entries, folding
//! the result into the diagnostics lines, and the signatures (file metadata,
//! task sources) that decide when a cached value is still current.
use super::*;

pub(super) fn canonicalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn spawn_command_refresh<F>(
    runtime: Arc<CompletionRuntime>,
    cache: Arc<RwLock<ProjectDynamicCache>>,
    cache_key: DynamicCommandCacheKey,
    loader: F,
) where
    F: FnOnce() -> Result<Vec<String>> + Send + 'static,
{
    let rejected_cache = cache.clone();
    let rejected_key = cache_key.clone();
    let job_runtime = runtime.clone();
    if !runtime.submit_command(Box::new(move || {
        let load_started = Instant::now();
        let result = loader();
        let load_duration = load_started.elapsed();
        let mut cache = cache.write();
        cache.command_pending.remove(&cache_key);
        match result {
            Ok(values) => {
                cache.command_errors.remove(&cache_key);
                cache.commands.insert(
                    cache_key,
                    CommandValueCacheEntry {
                        values,
                        cached_at: Instant::now(),
                        last_load_duration: Some(load_duration),
                        last_error: None,
                    },
                );
                prune_command_cache(&mut cache);
                update_diagnostics_from_cache(&job_runtime, &cache, None);
            }
            Err(err) => {
                warn!("Dynamic command completion refresh failed: {}", err);
                cache.command_errors.insert(
                    cache_key,
                    CommandValueErrorEntry {
                        recorded_at: Instant::now(),
                        last_load_duration: load_duration,
                        error: err.to_string(),
                    },
                );
                prune_command_error_cache(&mut cache);
                update_diagnostics_from_cache(&job_runtime, &cache, None);
            }
        }
        // Both success and error settle the pending refresh: notify exactly
        // once after the cache state is committed (and the write lock is
        // released) so a woken REPL re-reads settled state. The error branch
        // is safe to wake because `command_errors` + `error_backoff` suppress
        // an immediate re-refresh. Queue rejection stays outside this
        // contract: it has no error-backoff entry, so notifying there could
        // loop `queue full -> notify -> rerun -> queue full`.
        drop(cache);
        job_runtime.notify();
    })) {
        let mut cache = rejected_cache.write();
        cache.command_pending.remove(&rejected_key);
        runtime.record_queue_drop("dynamic command");
        update_diagnostics_from_cache(&runtime, &cache, None);
    }
}

pub(super) fn spawn_external_refresh<F>(
    runtime: Arc<CompletionRuntime>,
    cache: Arc<RwLock<ProjectDynamicCache>>,
    cache_key: ExternalCompletionCacheKey,
    loader: F,
) where
    F: FnOnce() -> Result<Vec<EnhancedCandidate>> + Send + 'static,
{
    let is_fish = cache_key.command_template.starts_with("fish-fallback:");
    let rejected_cache = cache.clone();
    let rejected_key = cache_key.clone();
    let job_runtime = runtime.clone();
    if !runtime.submit_external(
        is_fish,
        Box::new(move || {
            let result = loader();
            let mut cache = cache.write();
            cache.external_pending.remove(&cache_key);
            match result {
                Ok(candidates) => {
                    if candidates.is_empty() {
                        update_diagnostics_from_cache(
                            &job_runtime,
                            &cache,
                            Some("external refresh empty".to_string()),
                        );
                    } else {
                        insert_external_cache_entry(
                            &mut cache,
                            cache_key,
                            ExternalCompletionCacheEntry {
                                candidates,
                                cached_at: Instant::now(),
                            },
                        );
                        update_diagnostics_from_cache(
                            &job_runtime,
                            &cache,
                            Some("external refresh ok".to_string()),
                        );
                        job_runtime.notify();
                    }
                }
                Err(err) => {
                    warn!("External completer refresh failed: {}", err);
                    update_diagnostics_from_cache(
                        &job_runtime,
                        &cache,
                        Some(format!("external refresh error: {err}")),
                    );
                }
            }
        }),
    ) {
        let mut cache = rejected_cache.write();
        cache.external_pending.remove(&rejected_key);
        runtime.record_queue_drop(if is_fish { "fish" } else { "external" });
        update_diagnostics_from_cache(
            &runtime,
            &cache,
            Some("external refresh dropped: queue full".to_string()),
        );
    }
}

pub(super) fn insert_external_cache_entry(
    cache: &mut ProjectDynamicCache,
    cache_key: ExternalCompletionCacheKey,
    entry: ExternalCompletionCacheEntry,
) {
    cache.external.insert(cache_key, entry);
    prune_external_cache(cache);
}

pub(super) fn prune_command_cache(cache: &mut ProjectDynamicCache) {
    let overflow = cache
        .commands
        .len()
        .saturating_sub(DYNAMIC_COMMAND_CACHE_LIMIT);
    if overflow == 0 {
        return;
    }

    let mut keys = cache
        .commands
        .iter()
        .filter(|(key, _)| !cache.command_pending.contains(*key))
        .map(|(key, entry)| (key.clone(), entry.cached_at))
        .collect::<Vec<_>>();
    keys.sort_by_key(|(_, cached_at)| *cached_at);

    for (key, _) in keys.into_iter().take(overflow) {
        if cache.commands.remove(&key).is_some() {
            cache.command_errors.remove(&key);
            cache.command_pruned_total += 1;
        }
    }
}

pub(super) fn prune_command_error_cache(cache: &mut ProjectDynamicCache) {
    let overflow = cache
        .command_errors
        .len()
        .saturating_sub(DYNAMIC_COMMAND_CACHE_LIMIT);
    if overflow == 0 {
        return;
    }

    let mut keys = cache
        .command_errors
        .iter()
        .filter(|(key, _)| !cache.command_pending.contains(*key))
        .map(|(key, entry)| (key.clone(), entry.recorded_at))
        .collect::<Vec<_>>();
    keys.sort_by_key(|(_, recorded_at)| *recorded_at);

    for (key, _) in keys.into_iter().take(overflow) {
        if cache.command_errors.remove(&key).is_some() {
            cache.command_pruned_total += 1;
        }
    }
}

pub(super) fn prune_external_cache(cache: &mut ProjectDynamicCache) {
    let overflow = cache
        .external
        .len()
        .saturating_sub(EXTERNAL_COMPLETION_CACHE_LIMIT);
    if overflow == 0 {
        return;
    }

    let mut keys = cache
        .external
        .iter()
        .map(|(key, entry)| (key.clone(), entry.cached_at))
        .collect::<Vec<_>>();
    keys.sort_by_key(|(_, cached_at)| *cached_at);

    for (key, _) in keys.into_iter().take(overflow) {
        if cache.external.remove(&key).is_some() {
            cache.external_pruned_total += 1;
        }
    }
}

pub(super) fn update_diagnostics_from_cache(
    runtime: &CompletionRuntime,
    cache: &ProjectDynamicCache,
    last_external: Option<String>,
) {
    let mut diagnostics = runtime.diagnostics.write();
    diagnostics.command_entries = cache.commands.len();
    diagnostics.command_pending = cache.command_pending.len();
    diagnostics.command_pruned_total = cache.command_pruned_total;
    diagnostics.external_entries = cache.external.len();
    diagnostics.external_pending = cache.external_pending.len();
    diagnostics.external_fish_entries = cache
        .external
        .keys()
        .filter(|key| key.command_template.starts_with("fish-fallback:"))
        .count();
    diagnostics.external_pruned_total = cache.external_pruned_total;
    diagnostics.last_refresh = Some(Instant::now());
    diagnostics.provider_lines = provider_diagnostics_lines(cache);
    if let Some(last_external) = last_external {
        diagnostics.last_external = Some(last_external);
    }
}

pub(super) fn provider_diagnostics_lines(cache: &ProjectDynamicCache) -> Vec<String> {
    let mut keys = cache
        .commands
        .keys()
        .chain(cache.command_errors.keys())
        .chain(cache.command_pending.iter())
        .cloned()
        .collect::<Vec<_>>();
    keys.sort_by(|a, b| {
        dynamic_cache_kind_label(&a.kind)
            .cmp(&dynamic_cache_kind_label(&b.kind))
            .then_with(|| a.scope_dir.cmp(&b.scope_dir))
    });
    keys.dedup();
    keys.into_iter()
        .take(12)
        .map(|key| {
            let entry = cache.commands.get(&key);
            let error = cache.command_errors.get(&key);
            let pending = cache.command_pending.contains(&key);
            let values = entry.map(|entry| entry.values.len()).unwrap_or(0);
            let age = entry
                .map(|entry| format!("{}ms", entry.cached_at.elapsed().as_millis()))
                .or_else(|| error.map(|entry| format!("{}ms", entry.recorded_at.elapsed().as_millis())))
                .unwrap_or_else(|| "none".to_string());
            let duration = entry
                .and_then(|entry| entry.last_load_duration)
                .or_else(|| error.map(|entry| entry.last_load_duration))
                .map(|duration| format!("{}ms", duration.as_millis()))
                .unwrap_or_else(|| "unknown".to_string());
            let error_text = entry
                .and_then(|entry| entry.last_error.clone())
                .or_else(|| error.map(|entry| truncate_string(&entry.error, 80)))
                .unwrap_or_else(|| "none".to_string());
            format!(
                "completion-cache provider {} values={} pending={} age={} last-duration={} error={}",
                dynamic_cache_kind_label(&key.kind),
                values,
                pending,
                age,
                duration,
                error_text
            )
        })
        .collect()
}

pub(super) fn cached_value_matches(values: Vec<String>, current_token: &str) -> Vec<String> {
    if current_token.is_empty() {
        return values;
    }

    let mut prefix_matches = Vec::new();
    let mut fuzzy_candidates = Vec::new();
    for value in values {
        if value.starts_with(current_token) {
            prefix_matches.push(value);
        } else {
            fuzzy_candidates.push(value);
        }
    }

    if !prefix_matches.is_empty() {
        return prefix_matches;
    }

    fuzzy_candidates
        .into_iter()
        .filter(|value| matches_prefix(current_token, value))
        .collect()
}

pub(super) fn dynamic_cache_kind_label(kind: &DynamicCommandCacheKind) -> String {
    match kind {
        DynamicCommandCacheKind::GitBranch => "git.branch".to_string(),
        DynamicCommandCacheKind::GitRemote => "git.remote".to_string(),
        DynamicCommandCacheKind::GitWorktree => "git.worktree".to_string(),
        DynamicCommandCacheKind::KubectlContext => "kubectl.context".to_string(),
        DynamicCommandCacheKind::KubectlNamespace => "kubectl.namespace".to_string(),
        DynamicCommandCacheKind::CommandValue {
            command,
            value_kind,
        } => format!("{command}.{value_kind}"),
    }
}

pub(super) fn file_metadata_signature(path: &Path) -> FileMetadataSignature {
    match fs::metadata(path) {
        Ok(metadata) => FileMetadataSignature {
            exists: true,
            modified: metadata.modified().ok(),
            len: metadata.len(),
        },
        Err(_) => FileMetadataSignature {
            exists: false,
            modified: None,
            len: 0,
        },
    }
}

pub(super) fn task_completion_signature(
    project_root: &Path,
    sources: Option<&[&str]>,
) -> Vec<FileMetadataSignature> {
    let mut paths = [
        "mise.toml",
        "Taskfile.yml",
        "Taskfile.yaml",
        "turbo.json",
        "package.json",
        "Cargo.toml",
        "Makefile",
        "makefile",
        "deno.json",
        "deno.jsonc",
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "gradle.properties",
        "gradlew",
    ]
    .into_iter()
    .map(|name| project_root.join(name))
    .collect::<Vec<_>>();

    if sources_include_nx(sources) {
        paths.extend([
            project_root.join("workspace.json"),
            project_root.join("angular.json"),
            project_root.join("project.json"),
        ]);
        paths.extend(descendant_project_json_files(project_root, 4));
    }
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| file_metadata_signature(&path))
        .collect()
}

pub(super) fn sources_include_nx(sources: Option<&[&str]>) -> bool {
    sources.is_none_or(|sources| sources.contains(&"nx"))
}

pub(super) fn descendant_project_json_files(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_descendant_project_json_files(root, 0, max_depth, &mut paths);
    paths
}

pub(super) fn collect_descendant_project_json_files(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    paths: &mut Vec<PathBuf>,
) {
    if depth > max_depth {
        return;
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') || matches!(name, "node_modules" | "target" | "dist" | "build") {
            continue;
        }
        if path.is_file() && name == "project.json" {
            paths.push(path);
        } else if path.is_dir() {
            collect_descendant_project_json_files(&path, depth + 1, max_depth, paths);
        }
    }
}

pub(super) fn normalized_task_sources(sources: &[&str]) -> Vec<String> {
    let mut sources = sources
        .iter()
        .map(|source| (*source).to_string())
        .collect::<Vec<_>>();
    sources.sort();
    sources.dedup();
    sources
}
