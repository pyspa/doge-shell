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
