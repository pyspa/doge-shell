use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

fn write_executable(path: &Path) {
    fs::write(path, "").unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn candidate_texts(candidates: Vec<CompletionCandidate>) -> Vec<String> {
    candidates
        .into_iter()
        .map(|candidate| candidate.text)
        .collect()
}

fn command_set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

#[test]
fn system_command_candidates_scan_path() {
    let _guard = crate::test_env_lock();
    let dir = tempdir().unwrap();
    write_executable(&dir.path().join("shell-only-command"));
    let shell_paths = vec![dir.path().display().to_string()];

    let candidates = SystemCommandGenerator::new(&shell_paths)
        .generate_candidates("shell-")
        .expect("system candidates");
    let texts = candidate_texts(candidates);

    assert!(
        texts.contains(&"shell-only-command".to_string()),
        "expected explicit shell PATH command in {texts:?}"
    );
}

#[test]
fn path_switch_invalidates_candidate_cache() {
    let _guard = crate::test_env_lock();
    let dir = tempdir().unwrap();
    let path_a = dir.path().join("a");
    let path_b = dir.path().join("b");
    fs::create_dir(&path_a).unwrap();
    fs::create_dir(&path_b).unwrap();
    write_executable(&path_a.join("zz-old"));
    write_executable(&path_b.join("zz-new"));

    let paths_a = vec![path_a.display().to_string()];
    let first = candidate_texts(
        SystemCommandGenerator::new(&paths_a)
            .generate_candidates("zz-")
            .unwrap(),
    );
    assert!(first.contains(&"zz-old".to_string()));

    let paths_b = vec![path_b.display().to_string()];
    let second = candidate_texts(
        SystemCommandGenerator::new(&paths_b)
            .generate_candidates("zz-")
            .unwrap(),
    );
    assert!(second.contains(&"zz-new".to_string()));
    assert!(!second.contains(&"zz-old".to_string()));
}

#[test]
fn stale_request_ticket_never_reactivates_its_old_path() {
    let _guard = crate::test_env_lock();
    let dir = tempdir().unwrap();
    let path_a = dir.path().join("a");
    let path_b = dir.path().join("b");
    fs::create_dir(&path_a).unwrap();
    fs::create_dir(&path_b).unwrap();
    write_executable(&path_a.join("zz-old"));
    let paths_a = vec![path_a.display().to_string()];
    let paths_b = vec![path_b.display().to_string()];

    let old_a = activate_system_command_cache(&paths_a);
    let current_b = activate_system_command_cache(&paths_b);
    let candidates = SystemCommandGenerator::from_activation(old_a)
        .generate_candidates("zz-")
        .unwrap();
    assert!(candidate_texts(candidates).contains(&"zz-old".to_string()));
    assert_eq!(SYSTEM_COMMAND_CACHE.read().ticket(), current_b);
}

#[test]
fn stale_refresh_cannot_overwrite_newer_path() {
    let mut state = SystemCommandCacheState::new();
    let path_a = vec!["/logical/a".to_string()];
    let path_b = vec!["/logical/b".to_string()];
    let ticket_a = state.activate_paths(&path_a);
    let scan_a = state.next_scan_ticket(&ticket_a);
    let ticket_b = state.activate_paths(&path_b);
    let scan_b = state.next_scan_ticket(&ticket_b);

    assert!(state.publish_live(&scan_b, command_set(&["b-command"])));
    assert!(!state.publish_live(&scan_a, command_set(&["a-command"])));
    assert_eq!(state.commands, Some(command_set(&["b-command"])));
}

#[test]
fn path_generation_prevents_aba_publish() {
    let mut state = SystemCommandCacheState::new();
    let path_a = vec!["/logical/a".to_string()];
    let path_b = vec!["/logical/b".to_string()];
    let ticket_a1 = state.activate_paths(&path_a);
    let ticket_b = state.activate_paths(&path_b);
    let ticket_a2 = state.activate_paths(&path_a);

    assert_ne!(ticket_a1.generation, ticket_a2.generation);
    let scan_a1 = state.next_scan_ticket(&ticket_a1);
    let scan_a2 = state.next_scan_ticket(&ticket_a2);
    assert!(state.publish_live(&scan_a2, command_set(&["new-a"])));
    assert!(!state.publish_live(&scan_a1, command_set(&["old-a"])));
    assert_eq!(state.commands, Some(command_set(&["new-a"])));
    assert!(!state.is_current(&ticket_b));
}

#[test]
fn persistent_snapshot_cannot_overwrite_started_live_scan() {
    let mut state = SystemCommandCacheState::new();
    let activation = state.activate_paths(&["/logical/current".to_string()]);
    assert!(state.publish_cached(&activation, command_set(&["disk"]), Instant::now()));
    state.next_scan_ticket(&activation);
    assert!(!state.publish_cached(&activation, command_set(&["late-disk"]), Instant::now()));
    assert_eq!(state.commands, Some(command_set(&["disk"])));
}

#[test]
fn older_same_generation_scan_cannot_overwrite_newer_scan() {
    let mut state = SystemCommandCacheState::new();
    let ticket = state.activate_paths(&["/logical/current".to_string()]);
    let older = state.next_scan_ticket(&ticket);
    let newer = state.next_scan_ticket(&ticket);

    assert!(state.publish_live(&newer, command_set(&["newer"])));
    assert!(!state.publish_live(&older, command_set(&["older"])));
    assert_eq!(state.commands, Some(command_set(&["newer"])));
}

#[test]
fn stale_ttl_refresh_owns_logical_paths_without_sleeping() {
    let mut state = SystemCommandCacheState::new();
    let logical_paths = vec!["/logical/current".to_string()];
    let ticket = state.activate_paths(&logical_paths);
    let initial_scan = state.next_scan_ticket(&ticket);
    let now = Instant::now();
    assert!(state.publish_live_at(&initial_scan, command_set(&["current"]), now));

    let decision = state.plan_refresh_if_stale(
        &ticket,
        now + GLOBAL_SYSTEM_COMMAND_CACHE_TTL + Duration::from_secs(1),
    );
    let RefreshDecision::StartBackground(scan_ticket) = decision else {
        panic!("expected background refresh, got {decision:?}");
    };

    assert_eq!(scan_ticket.activation().paths.as_slice(), logical_paths);
    assert_eq!(state.inflight_scan_id, Some(scan_ticket.scan_id));
}

#[test]
fn candidate_cache_is_scoped_to_path_generation() {
    let mut state = SystemCommandCacheState::new();
    let ticket_a = state.activate_paths(&["/logical/a".to_string()]);
    let old = vec![CompletionCandidate::subcommand("zz-old".to_string(), None)];
    assert!(state.store_candidates(&ticket_a, "zz-", &old));
    assert_eq!(
        state.lookup_candidates(&ticket_a, "zz-").unwrap()[0].text,
        "zz-old"
    );

    let ticket_b = state.activate_paths(&["/logical/b".to_string()]);
    assert!(state.lookup_candidates(&ticket_b, "zz-").is_none());
    let new = vec![CompletionCandidate::subcommand("zz-new".to_string(), None)];
    assert!(state.store_candidates(&ticket_b, "zz-", &new));
    assert!(!state.store_candidates(&ticket_a, "zz-", &old));
    assert_eq!(
        state.lookup_candidates(&ticket_b, "zz-").unwrap()[0].text,
        "zz-new"
    );
}

#[test]
fn stale_worker_cannot_clear_new_workers_inflight_state() {
    let mut state = SystemCommandCacheState::new();
    let now = Instant::now();
    let ticket_a = state.activate_paths(&["/logical/a".to_string()]);
    let initial_a = state.next_scan_ticket(&ticket_a);
    assert!(state.publish_live_at(&initial_a, command_set(&["a"]), now));
    let RefreshDecision::StartBackground(scan_a) = state.plan_refresh_if_stale(
        &ticket_a,
        now + GLOBAL_SYSTEM_COMMAND_CACHE_TTL + Duration::from_secs(1),
    ) else {
        panic!("expected A background refresh");
    };

    let ticket_b = state.activate_paths(&["/logical/b".to_string()]);
    let initial_b = state.next_scan_ticket(&ticket_b);
    assert!(state.publish_live_at(&initial_b, command_set(&["b"]), now));
    let RefreshDecision::StartBackground(scan_b) = state.plan_refresh_if_stale(
        &ticket_b,
        now + GLOBAL_SYSTEM_COMMAND_CACHE_TTL + Duration::from_secs(1),
    ) else {
        panic!("expected B background refresh");
    };

    assert!(!state.publish_live(&scan_a, command_set(&["old-a"])));
    assert_eq!(state.inflight_scan_id, Some(scan_b.scan_id));
    assert!(state.publish_live(&scan_b, command_set(&["b"])));
    assert_eq!(state.inflight_scan_id, None);
}

#[test]
fn empty_runtime_paths_do_not_fallback_to_process_environment() {
    let _guard = crate::test_env_lock();
    let empty_dir = tempdir().unwrap();
    let sentinel_paths = vec![empty_dir.path().display().to_string()];
    let _ = SystemCommandGenerator::new(&sentinel_paths)
        .generate_candidates("sh")
        .unwrap();

    let candidates = SystemCommandGenerator::new(&[])
        .generate_candidates("sh")
        .expect("empty-path system candidates");
    assert!(
        candidates.is_empty(),
        "empty logical PATH discovered a process command: {candidates:?}"
    );
}

#[test]
fn cold_background_scan_returns_before_loader_release_and_coalesces() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "cold_background_scan_returns_before_loader_release_and_coalesces",
    ) {
        return;
    }
    let _guard = crate::test_env_lock();
    let dir = tempdir().unwrap();
    let generator = Arc::new(SystemCommandGenerator::new(&[dir
        .path()
        .display()
        .to_string()]));
    let runtime = Arc::new(crate::completion::dynamic::CompletionRuntime::new());
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel();
    runtime.set_notifier(notify_tx);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (returned_tx, returned_rx) = std::sync::mpsc::channel();
    let caller_generator = generator.clone();
    let caller_runtime = runtime.clone();
    let caller = std::thread::spawn(move || {
        let result =
            caller_generator.generate_in_background_with("zz-", caller_runtime, 100, move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                vec!["zz-ready".into()]
            });
        returned_tx.send(result).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let first = returned_rx.recv_timeout(Duration::from_millis(200));
    if first.is_err() {
        release_tx.send(()).unwrap();
        caller.join().unwrap();
        panic!("Tab waited for blocked PATH enumeration");
    }
    let first = first.unwrap();
    caller.join().unwrap();
    let scan_id = SYSTEM_COMMAND_CACHE.read().inflight_scan_id;
    let second = generator.generate_in_background_with("zz-", runtime.clone(), 100, |_| {
        panic!("repeated Tab must borrow the existing pending worker")
    });
    let same_scan = SYSTEM_COMMAND_CACHE.read().inflight_scan_id;
    release_tx.send(()).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(2), notify_rx.recv())
                .await
                .unwrap()
                .unwrap();
        });
    assert!(first.is_empty() && second.is_empty());
    assert_eq!(same_scan, scan_id);
    assert!(scan_id.is_some());
    assert_eq!(
        candidate_texts(generator.generate_in_background("zz-", runtime, 100)),
        vec!["zz-ready"]
    );
}

#[test]
fn background_claim_release_and_stale_snapshots_keep_their_owner() {
    let mut state = SystemCommandCacheState::new();
    let a = state.activate_paths(&["/cold/a".into()]);
    let first = state.plan_background(&a).unwrap();
    assert!(state.plan_background(&a).is_none());
    // Failed thread creation releases exactly the slot it claimed; a retry
    // receives a newer scan id. Publication order still rejects reverse finish.
    state.inflight_scan_id = None;
    let retry = state.plan_background(&a).unwrap();
    assert!(state.publish_live(&retry, command_set(&["new"])));
    assert!(!state.publish_live(&first, command_set(&["old"])));
    let b = state.activate_paths(&["/cold/b".into()]);
    assert!(state.plan_background(&a).is_none());
    let b_scan = state.plan_background(&b).unwrap();
    let a2 = state.activate_paths(&["/cold/a".into()]);
    assert!(state.plan_background(&a).is_none());
    assert!(!state.publish_live(&b_scan, command_set(&["b"])));
    assert!(!state.publish_live(&retry, command_set(&["old-a"])));
    assert!(state.plan_background(&a2).is_some());
}

#[test]
fn relative_and_empty_paths_are_anchored_before_worker_and_distinguish_cwd() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "relative_and_empty_paths_are_anchored_before_worker_and_distinguish_cwd",
    ) {
        return;
    }
    let _guard = crate::test_env_lock();
    let a = tempdir().unwrap();
    let b = tempdir().unwrap();
    let paths = vec!["bin".into(), "".into()];
    let filesystem = super::super::filesystem::FileSystemContext::new(a.path(), None);
    let activation = activate_system_command_cache_at(&paths, &filesystem);
    let generator = SystemCommandGenerator::from_activation(activation.clone());
    let runtime = Arc::new(crate::completion::dynamic::CompletionRuntime::new());
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    generator.generate_in_background_with("", runtime, 100, move |paths| {
        started_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        observed_tx.send(paths.to_vec()).unwrap();
        vec!["stale-from-a".into()]
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let other = activate_system_command_cache_at(
        &paths,
        &super::super::filesystem::FileSystemContext::new(b.path(), None),
    );
    assert_ne!(activation, other);
    release_tx.send(()).unwrap();
    let observed = observed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(observed, vec![a.path().join("bin"), a.path().join("")]);
    assert_eq!(SYSTEM_COMMAND_CACHE.read().ticket(), other);
    assert!(SYSTEM_COMMAND_CACHE.read().commands.is_none());
}

#[test]
fn failed_spawn_release_allows_retry_without_releasing_a_new_owner() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "failed_spawn_release_allows_retry_without_releasing_a_new_owner",
    ) {
        return;
    }
    let _guard = crate::test_env_lock();
    let dir = tempdir().unwrap();
    let activation = activate_system_command_cache(&[dir.path().display().to_string()]);
    let first = claim_background_system_command_scan(&activation).unwrap();
    assert!(claim_background_system_command_scan(&activation).is_none());
    // The real Builder::spawn Err branch uses this same release operation.
    release_system_command_scan(&first);
    let retry = claim_background_system_command_scan(&activation).unwrap();
    assert!(retry.scan_id > first.scan_id);
    release_system_command_scan(&first);
    assert!(claim_background_system_command_scan(&activation).is_none());
    release_system_command_scan(&retry);
    let final_claim = claim_background_system_command_scan(&activation).unwrap();
    release_system_command_scan(&final_claim);
}

#[test]
fn relative_path_activation_preserves_non_utf8_bytes_without_filesystem_support() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "relative_path_activation_preserves_non_utf8_bytes_without_filesystem_support",
    ) {
        return;
    }
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let _guard = crate::test_env_lock();
    let cwd = PathBuf::from(std::ffi::OsString::from_vec(b"/cwd-\xff".to_vec()));
    let paths = ["".into(), "bin".into()];
    let filesystem = super::super::filesystem::FileSystemContext::new(&cwd, None);
    let activation = activate_system_command_cache_at(&paths, &filesystem);
    let scan_paths = activation.scan_paths.as_ref().unwrap();
    assert_eq!(scan_paths[0].as_os_str().as_bytes(), b"/cwd-\xff/");
    assert_eq!(scan_paths[1].as_os_str().as_bytes(), b"/cwd-\xff/bin");
    assert_eq!(
        activation,
        activate_system_command_cache_at(&paths, &filesystem)
    );

    let lossy_cwd = PathBuf::from(cwd.to_string_lossy().into_owned());
    let lossy_activation = activate_system_command_cache_at(
        &paths,
        &super::super::filesystem::FileSystemContext::new(&lossy_cwd, None),
    );
    assert_ne!(activation, lossy_activation);
}

#[test]
fn relative_background_scan_preserves_non_utf8_request_directory() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "relative_background_scan_preserves_non_utf8_request_directory",
    ) {
        return;
    }
    use std::os::unix::ffi::OsStringExt;
    let _guard = crate::test_env_lock();
    let root = tempdir().unwrap();
    let cwd = root
        .path()
        .join(std::ffi::OsString::from_vec(b"cwd-\xff".to_vec()));
    if let Err(error) = fs::create_dir(&cwd) {
        // APFS can reject non-UTF-8 names. Keep the byte-identity contract in
        // the filesystem-independent test above on every supported platform.
        if error.raw_os_error() == Some(libc::EILSEQ) {
            return;
        }
        panic!("could not create non-UTF-8 cwd fixture: {error}");
    }
    fs::create_dir(cwd.join("bin")).unwrap();
    write_executable(&cwd.join("zz-empty-path"));
    write_executable(&cwd.join("bin/zz-relative-path"));
    let activation = activate_system_command_cache_at(
        &["".into(), "bin".into()],
        &super::super::filesystem::FileSystemContext::new(&cwd, None),
    );
    let generator = SystemCommandGenerator::from_activation(activation);
    let runtime = Arc::new(crate::completion::dynamic::CompletionRuntime::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    runtime.set_notifier(tx);
    generator.generate_in_background("zz-", runtime.clone(), 100);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
        });
    assert_eq!(
        candidate_texts(generator.generate_in_background("zz-", runtime, 100)),
        vec!["zz-empty-path", "zz-relative-path"]
    );
}

#[test]
fn scan_status_distinguishes_empty_success_release_and_partial_publication() {
    let mut state = SystemCommandCacheState::new();
    let activation = state.activate_paths(&["/fixture/status".into()]);
    let scan = state.next_background_scan_ticket(&activation);
    assert_eq!(state.scan_status().pending, Some(scan.scan_id));
    assert!(state.publish_live(&scan, BTreeSet::new()));
    let status = state.scan_status();
    assert_eq!(status.pending, None);
    assert_eq!(status.published, scan.scan_id);
    assert!(status.complete, "empty is a successful publication");
    let next = state.next_background_scan_ticket(&activation);
    assert_ne!(next.scan_id, status.latest_scan);
    state.inflight_scan_id = None;
    assert_ne!(
        state.scan_status().published,
        next.scan_id,
        "release is not readiness"
    );
}

#[test]
fn scan_status_incomplete_projection_is_not_ready() {
    let _guard = crate::test_env_lock();
    let dir = tempdir().unwrap();
    let activation = activate_system_command_cache(&[dir.path().display().to_string()]);
    let scan = begin_background_system_command_scan(&activation);
    assert!(publish_system_command_scan_if(
        &scan,
        command_set(&["partial"]),
        false,
        || true
    ));
    let status = system_command_scan_status();
    assert_eq!(status.published, scan.scan_id);
    assert!(!status.complete);
    assert_eq!(status.pending, None);
}
