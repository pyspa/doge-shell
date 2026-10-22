use super::*;
use crate::completion::dynamic::CompletionRuntime;
use crate::completion::dynamic::runtime::CompletionWorkerCounts;
use crate::environment::Environment;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use tempfile::TempDir;

fn fixture() -> (TempDir, DynamicCompletionProvider) {
    let dir = tempfile::Builder::new()
        .prefix("dogesh-task-refresh-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::write(dir.path().join("nx.json"), "{}\n").unwrap();
    let local_bin = dir.path().join("node_modules/.bin");
    fs::create_dir_all(&local_bin).unwrap();
    let nx = local_bin.join("nx");
    fs::write(&nx, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&nx).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&nx, permissions).unwrap();

    let environment = Environment::new_synthetic();
    environment.write().variable_state.paths.clear();
    (dir, DynamicCompletionProvider::new(environment))
}

fn task(name: &str) -> task::TaskInfo {
    task::TaskInfo::new("nx", name, format!("nx run {name}"), "/synthetic/root")
}

fn receiver() -> (
    tokio::sync::mpsc::UnboundedSender<()>,
    tokio::sync::mpsc::UnboundedReceiver<()>,
) {
    tokio::sync::mpsc::unbounded_channel()
}

fn wait_for_notification(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<()>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .expect("task refresh did not settle")
            .expect("completion notifier closed unexpectedly");
    });
}

#[test]
fn same_root_sources_and_nested_cwd_dedupe_pending_task_refresh() {
    let (dir, provider) = fixture();
    let nested = dir.path().join("apps/api");
    fs::create_dir_all(&nested).unwrap();
    let first = TaskDiscoveryRequest::new(&provider, &nested, &["nx", "npm"]);
    let reordered = TaskDiscoveryRequest::new(&provider, dir.path(), &["npm", "nx"]);
    assert_eq!(first.cache_key, reordered.cache_key);
    assert_eq!(first.signature, reordered.signature);

    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (notify_tx, mut notify_rx) = receiver();
    provider.runtime.set_notifier(notify_tx);
    assert!(
        queue_task_refresh(&provider, first, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(vec![task("api:build")])
        })
        .is_none()
    );
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("first task refresh did not start");

    let generation = provider.refresh_generation();
    let duplicate_loader_called = Arc::new(AtomicBool::new(false));
    let duplicate_loader_called_by_job = Arc::clone(&duplicate_loader_called);
    assert!(
        queue_task_refresh(&provider, reordered, move |_| {
            duplicate_loader_called_by_job.store(true, Ordering::Relaxed);
            Ok(Vec::new())
        })
        .is_none()
    );
    assert_eq!(provider.refresh_generation(), generation);
    assert!(provider.has_pending_refresh());

    release_tx.send(()).unwrap();
    wait_for_notification(&mut notify_rx);
    assert!(!duplicate_loader_called.load(Ordering::Relaxed));
    assert!(!provider.has_pending_refresh());
    let cache = provider.cache.read();
    assert_eq!(cache.tasks.len(), 1);
    assert_eq!(
        cache.tasks.values().next().unwrap().tasks[0].name,
        "api:build"
    );
    assert!(
        notify_rx.try_recv().is_err(),
        "one success should notify once"
    );
}

#[test]
fn superseded_queued_task_job_is_obsolete_before_discovery() {
    let (dir, provider) = fixture();
    let runtime = Arc::new(CompletionRuntime::with_worker_counts(
        CompletionWorkerCounts::new(1, 1, 1),
    ));
    let provider =
        DynamicCompletionProvider::with_runtime(provider.environment.clone(), runtime.clone());
    let (block_started_tx, block_started_rx) = mpsc::channel();
    let (block_release_tx, block_release_rx) = mpsc::channel();
    assert!(runtime.submit_command(Box::new(move || {
        block_started_tx.send(()).unwrap();
        block_release_rx.recv().unwrap();
    })));
    block_started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker blocker did not start");

    let first = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    let first_loader_called = Arc::new(AtomicBool::new(false));
    let first_loader_called_by_job = Arc::clone(&first_loader_called);
    assert!(
        queue_task_refresh(&provider, first, move |_| {
            first_loader_called_by_job.store(true, Ordering::Relaxed);
            Ok(vec![task("api:old")])
        })
        .is_none()
    );

    fs::write(dir.path().join("nx.json"), "{\"version\":1}\n").unwrap();
    let second = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    assert!(queue_task_refresh(&provider, second, |_| Ok(vec![task("api:new")])).is_none());

    let (notify_tx, mut notify_rx) = receiver();
    runtime.set_notifier(notify_tx);
    block_release_tx.send(()).unwrap();
    wait_for_notification(&mut notify_rx);

    assert!(!first_loader_called.load(Ordering::Relaxed));
    assert!(!provider.has_pending_refresh());
    let cache = provider.cache.read();
    let entry = cache.tasks.values().next().unwrap();
    assert_eq!(entry.tasks[0].name, "api:new");
    assert_eq!(cache.task_pending.len(), 0);
    assert!(
        notify_rx.try_recv().is_err(),
        "only the latest request should notify"
    );
}

#[test]
fn stale_settlement_cannot_remove_newer_pending_or_replace_cache() {
    let (dir, provider) = fixture();
    let mut older = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    older.ticket = 1;
    fs::write(dir.path().join("nx.json"), "{\"version\":1}\n").unwrap();
    let mut newer = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    newer.ticket = 2;
    assert_ne!(older.signature, newer.signature);

    provider.cache.write().task_pending.insert(
        newer.cache_key.clone(),
        TaskRefreshPending {
            signature: newer.signature,
            ticket: newer.ticket,
        },
    );
    let (notify_tx, mut notify_rx) = receiver();
    provider.runtime.set_notifier(notify_tx);

    assert!(!settle_task_refresh(
        &provider.cache,
        &provider.runtime,
        &older,
        Ok(Ok(vec![task("api:old")])),
    ));
    {
        let cache = provider.cache.read();
        assert_eq!(cache.task_pending[&newer.cache_key].ticket, newer.ticket);
        assert!(cache.tasks.is_empty());
    }
    assert!(notify_rx.try_recv().is_err());

    assert!(settle_task_refresh(
        &provider.cache,
        &provider.runtime,
        &newer,
        Ok(Ok(vec![task("api:new")])),
    ));
    let cache = provider.cache.read();
    assert!(cache.task_pending.is_empty());
    assert_eq!(cache.tasks[&newer.cache_key].tasks[0].name, "api:new");
    drop(cache);
    assert!(notify_rx.try_recv().is_ok());
    assert!(notify_rx.try_recv().is_err());
}

#[test]
fn task_refresh_errors_and_panics_clear_pending_and_back_off() {
    let (dir, provider) = fixture();
    let request = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    let (notify_tx, mut notify_rx) = receiver();
    provider.runtime.set_notifier(notify_tx);
    assert!(
        queue_task_refresh(&provider, request, |_| {
            Err(anyhow::anyhow!("synthetic provider failure"))
        })
        .is_none()
    );
    wait_for_notification(&mut notify_rx);
    let generation = provider.refresh_generation();
    assert!(!provider.has_pending_refresh());

    let same_inputs = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    let retry_called = Arc::new(AtomicBool::new(false));
    let retry_called_by_job = Arc::clone(&retry_called);
    assert!(
        queue_task_refresh(&provider, same_inputs, move |_| {
            retry_called_by_job.store(true, Ordering::Relaxed);
            Ok(vec![task("api:unexpected")])
        })
        .is_none()
    );
    assert_eq!(provider.refresh_generation(), generation);
    assert!(!provider.has_pending_refresh());
    assert!(!retry_called.load(Ordering::Relaxed));

    fs::write(dir.path().join("nx.json"), "{\"version\":2}\n").unwrap();
    let changed_inputs = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    let changed_generation = provider.refresh_generation();
    assert!(
        queue_task_refresh(&provider, changed_inputs, |_| {
            Ok(vec![task("api:changed")])
        })
        .is_none()
    );
    wait_for_notification(&mut notify_rx);
    assert_eq!(provider.refresh_generation(), changed_generation + 1);
    assert!(!provider.has_pending_refresh());
    assert!(notify_rx.try_recv().is_err());

    let mut panic_request = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    panic_request.ticket = 10;
    provider.cache.write().task_pending.insert(
        panic_request.cache_key.clone(),
        TaskRefreshPending {
            signature: panic_request.signature,
            ticket: panic_request.ticket,
        },
    );
    assert!(settle_task_refresh(
        &provider.cache,
        &provider.runtime,
        &panic_request,
        Err(Box::new(())),
    ));
    assert!(!provider.has_pending_refresh());
    assert!(notify_rx.try_recv().is_ok());
    assert!(notify_rx.try_recv().is_err());
}

#[test]
fn full_command_queue_clears_only_its_own_pending_without_notifying() {
    let (dir, base_provider) = fixture();
    let runtime = Arc::new(CompletionRuntime::with_worker_counts(
        CompletionWorkerCounts::new(1, 1, 1),
    ));
    let provider =
        DynamicCompletionProvider::with_runtime(base_provider.environment.clone(), runtime.clone());
    let (block_started_tx, block_started_rx) = mpsc::channel();
    let (block_release_tx, block_release_rx) = mpsc::channel();
    assert!(runtime.submit_command(Box::new(move || {
        block_started_tx.send(()).unwrap();
        block_release_rx.recv().unwrap();
    })));
    block_started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker blocker did not start");
    for _ in 0..4 {
        assert!(runtime.submit_command(Box::new(|| {})));
    }

    let (notify_tx, mut notify_rx) = receiver();
    runtime.set_notifier(notify_tx);
    let request = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    let loader_called = Arc::new(AtomicBool::new(false));
    let loader_called_by_job = Arc::clone(&loader_called);
    assert!(
        queue_task_refresh(&provider, request, move |_| {
            loader_called_by_job.store(true, Ordering::Relaxed);
            Ok(vec![task("api:unexpected")])
        })
        .is_none()
    );

    assert!(!provider.has_pending_refresh());
    assert_eq!(provider.runtime.diagnostics.read().queue_dropped_total, 1);
    assert!(!loader_called.load(Ordering::Relaxed));
    assert!(notify_rx.try_recv().is_err());
    block_release_tx.send(()).unwrap();
}

#[test]
fn task_refresh_error_metadata_is_bounded() {
    let (dir, provider) = fixture();
    let request = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    let mut cache = super::super::ProjectDynamicCache::default();
    let now = Instant::now();
    for index in 0..=TASK_REFRESH_ERROR_LIMIT {
        let key = TaskCacheKey {
            project_root: PathBuf::from(format!("/synthetic/project-{index}")),
            sources: vec!["nx".to_string()],
        };
        cache.task_errors.insert(
            key,
            TaskRefreshErrorEntry {
                signature: request.signature,
                recorded_at: now - Duration::from_secs((TASK_REFRESH_ERROR_LIMIT - index) as u64),
            },
        );
    }
    prune_task_refresh_errors(&mut cache);
    assert_eq!(cache.task_errors.len(), TASK_REFRESH_ERROR_LIMIT);
}

#[test]
fn cached_only_task_lookup_never_queues_cold_pending_or_changed_signature() {
    let (dir, provider) = fixture();
    let generation = provider.refresh_generation();
    assert!(
        provider
            .lookup_project_tasks_for_sources(dir.path(), &["nx"])
            .is_empty()
    );
    assert_eq!(provider.refresh_generation(), generation);
    assert!(!provider.has_pending_refresh());

    let request = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    provider.cache.write().task_pending.insert(
        request.cache_key.clone(),
        TaskRefreshPending {
            signature: request.signature,
            ticket: 1,
        },
    );
    assert!(
        provider
            .lookup_project_tasks_for_sources(dir.path(), &["nx"])
            .is_empty()
    );
    assert_eq!(provider.refresh_generation(), generation);
    provider.cache.write().task_pending.clear();
    provider.cache.write().tasks.insert(
        request.cache_key,
        TaskCacheEntry {
            signature: request.signature,
            tasks: vec![task("api:old")],
        },
    );
    fs::write(dir.path().join("nx.json"), "{\"version\":2}\n").unwrap();
    assert!(
        provider
            .lookup_project_tasks_for_sources(dir.path(), &["nx"])
            .is_empty()
    );
    assert_eq!(provider.refresh_generation(), generation);
    assert!(!provider.has_pending_refresh());
}

#[test]
fn switching_to_static_nx_supersedes_pending_cli_refresh() {
    let (dir, fixture_provider) = fixture();
    let runtime = Arc::new(CompletionRuntime::with_worker_counts(
        CompletionWorkerCounts::new(1, 1, 1),
    ));
    let provider = DynamicCompletionProvider::with_runtime(
        fixture_provider.environment.clone(),
        runtime.clone(),
    );
    fs::write(
        dir.path().join("project.json"),
        r#"{"name":"api","targets":{"static":{}}}"#,
    )
    .unwrap();
    let request = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (notify_tx, mut notify_rx) = receiver();
    runtime.set_notifier(notify_tx);
    queue_task_refresh(&provider, request, move |_| {
        started_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        Ok(vec![task("api:old")])
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    fs::remove_file(dir.path().join("node_modules/.bin/nx")).unwrap();
    let tasks = provider
        .load_or_schedule_project_tasks_for_sources(dir.path(), &["nx"])
        .unwrap()
        .unwrap();
    // Release before assertions so a failing regression cannot strand a worker.
    release_tx.send(()).unwrap();
    let (barrier_tx, barrier_rx) = mpsc::channel();
    assert!(runtime.submit_command(Box::new(move || barrier_tx.send(()).unwrap())));
    barrier_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(tasks.iter().any(|task| task.name == "api:static"));
    assert!(!provider.has_pending_refresh());
    let cached = provider.lookup_project_tasks_for_sources(dir.path(), &["nx"]);
    assert!(
        cached.iter().any(|task| task.name == "api:static"),
        "{cached:?}"
    );
    assert!(
        notify_rx.try_recv().is_err(),
        "superseded CLI job must not notify"
    );
}

#[test]
fn actual_nx_cli_failure_preserves_soft_static_fallback() {
    let (dir, provider) = fixture();
    fs::write(
        dir.path().join("node_modules/.bin/nx"),
        "#!/bin/sh\nexit 1\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        r#"{"name":"api","targets":{"static":{}}}"#,
    )
    .unwrap();
    let (notify_tx, mut notify_rx) = receiver();
    provider.runtime.set_notifier(notify_tx);
    assert!(
        provider
            .load_or_schedule_project_tasks_for_sources(dir.path(), &["nx"])
            .unwrap()
            .is_none()
    );
    wait_for_notification(&mut notify_rx);
    assert!(
        provider
            .lookup_project_tasks_for_sources(dir.path(), &["nx"])
            .iter()
            .any(|task| task.name == "api:static")
    );
    assert!(provider.cache.read().task_errors.is_empty());
    assert!(!provider.has_pending_refresh());
}

#[test]
fn synchronous_discovery_cannot_overwrite_newer_already_settled_refresh() {
    let (dir, provider) = fixture();
    let older = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    let (notify_tx, mut notify_rx) = receiver();
    provider.runtime.set_notifier(notify_tx);
    let result = load_task_discovery_request_with_loader(&provider, older, |_| {
        fs::write(dir.path().join("nx.json"), "{\"new\":true}").unwrap();
        let newer = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
        queue_task_refresh(&provider, newer, |_| Ok(vec![task("api:new")]));
        wait_for_notification(&mut notify_rx);
        assert!(!provider.has_pending_refresh());
        Ok(vec![task("api:old")])
    })
    .unwrap();
    assert_eq!(result[0].name, "api:old");
    assert_eq!(
        provider.lookup_project_tasks_for_sources(dir.path(), &["nx"])[0].name,
        "api:new"
    );
}

#[test]
fn pending_refresh_dedupes_across_epoch_and_settles_with_current_epoch() {
    let (dir, provider) = fixture();
    let first = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    let signature = first.signature;
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (notify_tx, mut notify_rx) = receiver();
    provider.runtime.set_notifier(notify_tx);
    queue_task_refresh(&provider, first, move |_| {
        started_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        Ok(vec![task("api:build")])
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let until_next_epoch = Duration::from_millis(5010 - (since_epoch.as_millis() % 5000) as u64);
    let (_tx, rx) = mpsc::channel::<()>();
    let _ = rx.recv_timeout(until_next_epoch);
    let second = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
    let new_signature = second.signature;
    let generation = provider.refresh_generation();
    let duplicate_called = Arc::new(AtomicBool::new(false));
    let duplicate_in_job = duplicate_called.clone();
    queue_task_refresh(&provider, second, move |_| {
        duplicate_in_job.store(true, Ordering::Relaxed);
        Ok(Vec::new())
    });
    release_tx.send(()).unwrap();
    wait_for_notification(&mut notify_rx);
    assert_ne!(signature, new_signature);
    assert!(signature.has_same_inputs_as(&new_signature));
    assert_eq!(provider.refresh_generation(), generation);
    assert!(!duplicate_called.load(Ordering::Relaxed));
    assert_eq!(
        provider.lookup_project_tasks_for_sources(dir.path(), &["nx"])[0].name,
        "api:build"
    );
    assert!(notify_rx.try_recv().is_err());
}
