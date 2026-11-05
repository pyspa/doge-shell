use super::*;
use crate::environment::Environment;
use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::{Barrier, mpsc};
fn environment(path: &str) -> Arc<RwLock<Environment>> {
    let env = Environment::new_synthetic();
    {
        let mut e = env.write();
        e.variable_state.paths = vec![path.into()];
        e.isolated_projection = false;
        e.invocation.interactive = true;
    }
    env
}
#[test]
fn pending_search_returns_before_the_scan_barrier_and_ready_empty_stays_ready() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "pending_search_returns_before_the_scan_barrier_and_ready_empty_stays_ready",
    ) {
        return;
    }
    let _guard = crate::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let pending = dir.path().join("pending-target");
    std::fs::write(&pending, "").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = environment(dir.path().to_str().unwrap());
    assert!(env.read().lookup("pending-target").is_some());
    let gate = Arc::new(Barrier::new(2));
    let worker_gate = gate.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let worker = env
        .read()
        .start_path_ghost_scan(move |_| {
            started_tx.send(()).unwrap();
            worker_gate.wait();
            Ok(Vec::new())
        })
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = env.clone();
    let input = std::thread::spawn(move || {
        tx.send(reader.read().search_prefix("pending-")).unwrap();
    });
    let result = rx.recv_timeout(Duration::from_millis(200));
    gate.wait();
    worker.join().unwrap();
    input.join().unwrap();
    assert_eq!(result.unwrap(), None, "input must not wait for the scan");
    assert!(matches!(
        env.read().completion_state.path_ghost.lock().phase,
        Phase::Ready(_)
    ));
    assert_eq!(env.read().search_prefix("pending-"), None);
}
#[test]
fn a_b_a_and_reverse_scan_completion_cannot_publish_old_names() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "a_b_a_and_reverse_scan_completion_cannot_publish_old_names",
    ) {
        return;
    }
    let _guard = crate::test_env_lock();
    let env = environment("/ghost-a");
    let gate = Arc::new(Barrier::new(2));
    let old_gate = gate.clone();
    let old = env
        .read()
        .start_path_ghost_scan(move |_| {
            old_gate.wait();
            Ok(vec!["old-a".into()])
        })
        .unwrap();
    env.write().variable_state.paths = vec!["/ghost-b".into()];
    env.read()
        .start_path_ghost_scan(|_| Ok(vec!["b".into()]))
        .unwrap()
        .join()
        .unwrap();
    env.write().variable_state.paths = vec!["/ghost-a".into()];
    env.read()
        .start_path_ghost_scan(|_| Ok(vec!["new-a".into()]))
        .unwrap()
        .join()
        .unwrap();
    gate.wait();
    old.join().unwrap();
    assert_eq!(env.read().search_prefix("new-"), Some("new-a".into()));
    assert_eq!(env.read().search_prefix("old-"), None);
    let gate = Arc::new(Barrier::new(2));
    let old_gate = gate.clone();
    let old = env
        .read()
        .start_path_ghost_scan(move |_| {
            old_gate.wait();
            Ok(vec!["same-old".into()])
        })
        .unwrap();
    env.read()
        .start_path_ghost_scan(|_| Ok(vec!["same-new".into()]))
        .unwrap()
        .join()
        .unwrap();
    gate.wait();
    old.join().unwrap();
    assert_eq!(env.read().search_prefix("same-"), Some("same-new".into()));
}
#[test]
fn failed_scan_can_retry_and_completion_before_registration_is_replayed() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "failed_scan_can_retry_and_completion_before_registration_is_replayed",
    ) {
        return;
    }
    let _guard = crate::test_env_lock();
    let env = environment("/ghost-retry");
    env.read()
        .start_path_ghost_scan(|_| anyhow::bail!("fixture failure"))
        .unwrap()
        .join()
        .unwrap();
    assert!(matches!(
        env.read().completion_state.path_ghost.lock().phase,
        Phase::Failed(_)
    ));
    {
        let e = env.read();
        e.completion_state.path_ghost.lock().phase =
            Phase::Failed(Instant::now() - Duration::from_secs(1));
    }
    env.read()
        .start_path_ghost_scan(|_| Ok(vec!["recovered".into()]))
        .unwrap()
        .join()
        .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    env.read().set_path_ghost_notifier(tx);
    assert!(rx.try_recv().is_ok());
    assert_eq!(env.read().search_prefix("rec"), Some("recovered".into()));
}

#[test]
fn cold_and_ready_keep_their_documented_order_exact_and_symlink_semantics() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "cold_and_ready_keep_their_documented_order_exact_and_symlink_semantics",
    ) {
        return;
    }
    use std::os::unix::fs::{PermissionsExt, symlink};
    let _guard = crate::test_env_lock();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    for path in [a.path().join("compat-z"), b.path().join("compat-a")] {
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    symlink("compat-z", a.path().join("link-target")).unwrap();
    symlink("missing", a.path().join("broken-link")).unwrap();
    let env = environment(a.path().to_str().unwrap());
    env.write()
        .variable_state
        .paths
        .push(b.path().display().to_string());
    // Legacy cold lookup was PATH-first and skipped non-exact symlinks.
    assert_eq!(env.read().search("compat-"), Some("compat-z".into()));
    assert_eq!(env.read().search("link-"), None);
    assert_eq!(env.read().search("link-target"), Some("link-target".into()));
    env.read()
        .start_path_ghost_scan(super::super::collect_executables_checked)
        .unwrap()
        .join()
        .unwrap();
    // Deferred cold ghosts use the existing ready projection, globally sorted.
    assert_eq!(env.read().search_prefix("compat-"), Some("compat-a".into()));
    assert_eq!(
        env.read().search_prefix("compat-z"),
        Some("compat-z".into())
    );
    assert_eq!(
        env.read().search_prefix("link-"),
        Some("link-target".into())
    );
    assert_eq!(
        env.read().search_prefix("broken-"),
        Some("broken-link".into())
    );
    env.write().variable_state.paths = vec![".".into()];
    assert_eq!(
        env.read().search_prefix("compat-"),
        env.read().search("compat-")
    );
    env.write().variable_state.paths = vec![String::new()];
    assert_eq!(
        env.read().search_prefix("compat-"),
        env.read().search("compat-")
    );
}

#[test]
fn retry_after_failure_is_scheduled_without_another_key_and_not_before_backoff() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "retry_after_failure_is_scheduled_without_another_key_and_not_before_backoff",
    ) {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let _guard = crate::test_env_lock();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let env = environment(dir.path().to_str().unwrap());
            env.read()
                .start_path_ghost_scan(|_| anyhow::bail!("blocked fixture"))
                .unwrap()
                .join()
                .unwrap();
            let exe = dir.path().join("retry-command");
            std::fs::write(&exe, "").unwrap();
            std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(env.read().search_prefix("retry-"), None);
            assert!(matches!(
                env.read().completion_state.path_ghost.lock().phase,
                Phase::Failed(_)
            ));
            {
                let e = env.read();
                e.completion_state.path_ghost.lock().phase =
                    Phase::Failed(Instant::now() - Duration::from_secs(1));
            }
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            env.read().set_path_ghost_notifier(tx);
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    rx.recv().await.unwrap();
                    if matches!(
                        env.read().completion_state.path_ghost.lock().phase,
                        Phase::Ready(_)
                    ) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(
                env.read().search_prefix("retry-"),
                Some("retry-command".into())
            );
        });
}

#[test]
fn noninteractive_lookup_keeps_legacy_behavior_without_starting_a_worker() {
    let _guard = crate::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let env = environment(dir.path().to_str().unwrap());
    env.write().invocation.interactive = false;
    env.read().ensure_path_ghost_scan();
    assert!(matches!(
        env.read().completion_state.path_ghost.lock().phase,
        Phase::Uninitialized
    ));
    assert_eq!(
        env.read().search_prefix("missing"),
        env.read().search("missing")
    );
}

#[test]
fn unreadable_path_element_keeps_partial_names_and_retries_after_recovery() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "unreadable_path_element_keeps_partial_names_and_retries_after_recovery",
    ) {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let _guard = crate::test_env_lock();
    let good = tempfile::tempdir().unwrap();
    let blocked = tempfile::tempdir().unwrap();
    let command = good.path().join("partial-command");
    std::fs::write(&command, "").unwrap();
    std::fs::set_permissions(command, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = environment(good.path().to_str().unwrap());
    env.write()
        .variable_state
        .paths
        .push(blocked.path().display().to_string());
    std::fs::set_permissions(blocked.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
    let worker = env
        .read()
        .start_path_ghost_scan(super::super::collect_executables_checked)
        .unwrap();
    worker.join().unwrap();
    std::fs::set_permissions(blocked.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        env.read().search_prefix("partial-"),
        Some("partial-command".into())
    );
    assert!(matches!(
        env.read().completion_state.path_ghost.lock().phase,
        Phase::Failed(_)
    ));
    env.read()
        .start_path_ghost_scan(super::super::collect_executables_checked)
        .unwrap()
        .join()
        .unwrap();
    assert!(matches!(
        env.read().completion_state.path_ghost.lock().phase,
        Phase::Ready(_)
    ));
}

#[test]
fn changing_to_relative_or_empty_path_invalidates_the_old_worker() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "changing_to_relative_or_empty_path_invalidates_the_old_worker",
    ) {
        return;
    }
    let _guard = crate::test_env_lock();
    for next in [".", ""] {
        let env = environment("/old-absolute-ghost");
        let gate = Arc::new(Barrier::new(2));
        let old_gate = gate.clone();
        let old = env
            .read()
            .start_path_ghost_scan(move |_| {
                old_gate.wait();
                Ok(vec!["obsolete".into()])
            })
            .unwrap();
        let ticket = env
            .read()
            .completion_state
            .path_ghost
            .lock()
            .ticket
            .clone()
            .unwrap();
        env.write().set_shell_var("PATH".into(), next.into());
        let pending = env.read().path_ghost_pending();
        let current = generator::system_command_scan_is_current(&ticket);
        gate.wait();
        old.join().unwrap();
        assert!(!pending, "relative PATH must not keep old pending status");
        assert!(
            !current,
            "relative activation must invalidate the old global ticket"
        );
        assert!(
            env.read()
                .completion_state
                .executable_names
                .read()
                .is_empty()
        );
    }
}

#[test]
fn ordinary_ghost_refresh_coalesces_with_tab_and_clears_previous_path_projection() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "ordinary_ghost_refresh_coalesces_with_tab_and_clears_previous_path_projection",
    ) {
        return;
    }
    let _guard = crate::test_env_lock();
    let old = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let env = environment(old.path().to_str().unwrap());
    env.write()
        .set_executable_names(vec!["old-path-name".into()]);
    let paths = vec![new.path().display().to_string()];
    let activation = generator::activate_system_command_cache(&paths);
    let tab =
        crate::completion::generators::system::claim_background_system_command_scan(&activation)
            .unwrap();
    env.write().variable_state.paths = paths;
    assert!(
        env.read()
            .start_path_ghost_refresh(|_| panic!("Tab already owns this activation"))
            .is_none()
    );
    assert!(
        env.read()
            .completion_state
            .executable_names
            .read()
            .is_empty()
    );
    assert!(
        crate::completion::generators::system::claim_background_system_command_scan(&activation)
            .is_none()
    );
    generator::release_system_command_scan(&tab);
    env.read()
        .start_path_ghost_refresh(|_| Ok(vec!["new-path-name".into()]))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(
        env.read().search_prefix("new-"),
        Some("new-path-name".into())
    );
}
