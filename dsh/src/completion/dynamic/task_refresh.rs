//! Nonblocking cache refresh for CLI-backed Nx project task completion.
//!
//! The request owns the canonical project root, normalized source filter,
//! logical shell runtime snapshot, signature, and refresh ticket. Worker jobs
//! never reread shell state or process-global environment.

use super::{
    DynamicCompletionProvider, TaskCacheEntry, TaskCacheKey, TaskRefreshErrorEntry,
    TaskRefreshPending, canonicalize_path, normalized_task_sources, update_diagnostics_from_cache,
};
use anyhow::Result;
use dsh_builtin::task;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const TASK_REFRESH_ERROR_BACKOFF: Duration = Duration::from_secs(2);
const TASK_REFRESH_ERROR_LIMIT: usize = 64;

struct TaskDiscoveryRequest {
    cache_key: TaskCacheKey,
    project_root: PathBuf,
    sources: Vec<String>,
    runtime: task::TaskDiscoveryRuntime,
    signature: task::TaskDiscoverySignature,
    ticket: u64,
}

impl TaskDiscoveryRequest {
    fn new(provider: &DynamicCompletionProvider, current_dir: &Path, sources: &[&str]) -> Self {
        let project_root = canonicalize_path(&provider.cached_project_root(current_dir));
        let sources = normalized_task_sources(sources);
        let runtime = provider.task_discovery_runtime();
        let source_refs = sources.iter().map(String::as_str).collect::<Vec<_>>();
        let signature = task::discovery_signature(&project_root, Some(&source_refs), &runtime);
        let cache_key = TaskCacheKey {
            project_root: project_root.clone(),
            sources: sources.clone(),
        };
        Self {
            cache_key,
            project_root,
            sources,
            runtime,
            signature,
            ticket: 0,
        }
    }

    fn discover(&self) -> Result<Vec<task::TaskInfo>> {
        let sources = self.sources.iter().map(String::as_str).collect::<Vec<_>>();
        task::list_tasks_in_dir_for_sources(&self.project_root, &sources, &self.runtime)
    }
}

impl DynamicCompletionProvider {
    pub(super) fn load_project_tasks_for_sources(
        &self,
        current_dir: &Path,
        sources: &[&str],
    ) -> Result<Vec<task::TaskInfo>> {
        let request = TaskDiscoveryRequest::new(self, current_dir, sources);
        load_task_discovery_request(self, request)
    }

    /// Load static Nx task definitions synchronously and queue only a
    /// CLI-backed Nx cache miss or expiration.
    pub(super) fn load_or_schedule_project_tasks_for_sources(
        &self,
        current_dir: &Path,
        sources: &[&str],
    ) -> Result<Option<Vec<task::TaskInfo>>> {
        let request = TaskDiscoveryRequest::new(self, current_dir, sources);
        if !request.signature.is_nx_cli_backed() {
            return load_task_discovery_request(self, request).map(Some);
        }

        Ok(queue_task_refresh(
            self,
            request,
            TaskDiscoveryRequest::discover,
        ))
    }
}

fn load_task_discovery_request(
    provider: &DynamicCompletionProvider,
    request: TaskDiscoveryRequest,
) -> Result<Vec<task::TaskInfo>> {
    load_task_discovery_request_with_loader(provider, request, TaskDiscoveryRequest::discover)
}

fn load_task_discovery_request_with_loader<F>(
    provider: &DynamicCompletionProvider,
    request: TaskDiscoveryRequest,
    loader: F,
) -> Result<Vec<task::TaskInfo>>
where
    F: FnOnce(&TaskDiscoveryRequest) -> Result<Vec<task::TaskInfo>>,
{
    let sync_ticket;
    {
        let mut cache = provider.cache.write();
        sync_ticket = request
            .sources
            .iter()
            .any(|source| source == "nx")
            .then(|| {
                provider
                    .task_refresh_ticket
                    .fetch_add(1, Ordering::Relaxed)
                    .wrapping_add(1)
            });
        // A synchronous request (including CLI -> static Nx) supersedes any
        // older worker for this key before it can publish or notify.
        cache.task_pending.remove(&request.cache_key);
        cache.task_errors.remove(&request.cache_key);
        if let Some(entry) = cache.tasks.get(&request.cache_key)
            && entry.signature == request.signature
        {
            return Ok(entry.tasks.clone());
        }
    }

    let tasks = loader(&request)?;
    let mut cache = provider.cache.write();
    // A newer task request can have both queued and settled during static
    // loading. Ticket equality protects that completed result as well.
    if !cache.task_pending.contains_key(&request.cache_key)
        && sync_ticket
            .is_none_or(|ticket| provider.task_refresh_ticket.load(Ordering::Relaxed) == ticket)
    {
        cache.tasks.insert(
            request.cache_key,
            TaskCacheEntry {
                signature: request.signature.after_discovery(),
                tasks: tasks.clone(),
            },
        );
    }
    Ok(tasks)
}

fn queue_task_refresh<F>(
    provider: &DynamicCompletionProvider,
    mut request: TaskDiscoveryRequest,
    loader: F,
) -> Option<Vec<task::TaskInfo>>
where
    F: FnOnce(&TaskDiscoveryRequest) -> Result<Vec<task::TaskInfo>> + Send + 'static,
{
    let ticket;
    let cache_key = request.cache_key.clone();

    {
        let mut cache = provider.cache.write();
        if let Some(entry) = cache.tasks.get(&cache_key)
            && entry.signature == request.signature
        {
            return Some(entry.tasks.clone());
        }

        if cache
            .task_pending
            .get(&cache_key)
            .is_some_and(|pending| pending.signature.has_same_inputs_as(&request.signature))
        {
            return None;
        }

        if cache.task_errors.get(&cache_key).is_some_and(|error| {
            error.signature.has_same_inputs_as(&request.signature)
                && error.recorded_at.elapsed() < TASK_REFRESH_ERROR_BACKOFF
        }) {
            return None;
        }

        ticket = provider
            .task_refresh_ticket
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        request.ticket = ticket;
        cache.task_errors.remove(&cache_key);
        cache.task_pending.insert(
            cache_key.clone(),
            TaskRefreshPending {
                signature: request.signature,
                ticket,
            },
        );
        provider.mark_refresh_scheduled();
    }

    let rejected_key = request.cache_key.clone();
    let job_cache = provider.cache.clone();
    let job_runtime = provider.runtime.clone();
    let rejected_cache = provider.cache.clone();
    let rejected_runtime = provider.runtime.clone();
    let accepted = provider.runtime.submit_command(Box::new(move || {
        if !job_cache
            .read()
            .task_pending
            .get(&cache_key)
            .is_some_and(|pending| {
                pending.ticket == request.ticket && pending.signature == request.signature
            })
        {
            return;
        }

        let result = catch_unwind(AssertUnwindSafe(|| loader(&request)));
        settle_task_refresh(&job_cache, &job_runtime, &request, result);
    }));

    if !accepted {
        let mut cache = rejected_cache.write();
        if cache
            .task_pending
            .get(&rejected_key)
            .is_some_and(|pending| pending.ticket == ticket)
        {
            cache.task_pending.remove(&rejected_key);
        }
        rejected_runtime.record_queue_drop("project task");
        update_diagnostics_from_cache(&rejected_runtime, &cache, None);
    }

    None
}

fn settle_task_refresh(
    cache: &parking_lot::RwLock<super::ProjectDynamicCache>,
    runtime: &super::CompletionRuntime,
    request: &TaskDiscoveryRequest,
    result: std::thread::Result<Result<Vec<task::TaskInfo>>>,
) -> bool {
    let mut cache = cache.write();
    let current = cache
        .task_pending
        .get(&request.cache_key)
        .is_some_and(|pending| {
            pending.ticket == request.ticket && pending.signature == request.signature
        });
    if !current {
        return false;
    }

    cache.task_pending.remove(&request.cache_key);
    match result {
        Ok(Ok(tasks)) => {
            cache.task_errors.remove(&request.cache_key);
            cache.tasks.insert(
                request.cache_key.clone(),
                TaskCacheEntry {
                    signature: request.signature.after_discovery(),
                    tasks,
                },
            );
        }
        Ok(Err(_)) | Err(_) => {
            cache.task_errors.insert(
                request.cache_key.clone(),
                TaskRefreshErrorEntry {
                    signature: request.signature,
                    recorded_at: Instant::now(),
                },
            );
            prune_task_refresh_errors(&mut cache);
            tracing::warn!("Nx project task refresh failed; backoff recorded");
        }
    }
    update_diagnostics_from_cache(runtime, &cache, None);
    drop(cache);
    runtime.notify();
    true
}

fn prune_task_refresh_errors(cache: &mut super::ProjectDynamicCache) {
    let overflow = cache
        .task_errors
        .len()
        .saturating_sub(TASK_REFRESH_ERROR_LIMIT);
    if overflow == 0 {
        return;
    }

    let mut oldest = cache
        .task_errors
        .iter()
        .map(|(key, entry)| (key.clone(), entry.recorded_at))
        .collect::<Vec<_>>();
    oldest.sort_by_key(|(_, recorded_at)| *recorded_at);
    for (key, _) in oldest.into_iter().take(overflow) {
        cache.task_errors.remove(&key);
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod probes;
