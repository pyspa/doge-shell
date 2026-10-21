//! Regression tests for provider admission, pipe capture, and cache expiration.
use super::*;
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::PermissionsExt,
    time::{SystemTime, UNIX_EPOCH},
};
use tempfile::tempdir;

fn executable(path: &Path, script: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
fn empty_runtime() -> TaskDiscoveryRuntime {
    TaskDiscoveryRuntime::new(Vec::new(), HashMap::new())
}
fn local_nx(root: &Path) {
    fs::write(root.join("nx.json"), "{}").unwrap();
    executable(
        &root.join("node_modules/.bin/nx"),
        "#!/bin/sh\nprintf 'called\\n' >> provider-ran\ncase \"$2\" in projects) printf '[\"api\"]';; project) printf '{\"targets\":{\"dynamic\":{}}}';; esac\n",
    );
}

#[test]
fn metadata_only_never_starts_local_nx() {
    let dir = tempdir().unwrap();
    local_nx(dir.path());
    let summary = summarize_tasks_in_dir_metadata_only(dir.path()).unwrap();
    assert!(!dir.path().join("provider-ran").exists());
    assert!(summary.tasks.is_empty());
}

#[test]
fn source_filter_does_not_start_unrelated_nx() {
    let dir = tempdir().unwrap();
    local_nx(dir.path());
    fs::write(dir.path().join("turbo.json"), r#"{"tasks":{"build":{}}}"#).unwrap();
    let tasks = list_tasks_in_dir_for_sources(dir.path(), &["turbo"], &empty_runtime()).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].name, "build");
    assert!(!dir.path().join("provider-ran").exists());
}

fn capture_script(script: &str) -> Result<std::process::Output> {
    let dir = tempdir().unwrap();
    let path = dir.path().join("provider");
    executable(&path, script);
    command_output_with_timeout(
        &path,
        &[],
        dir.path(),
        Duration::from_millis(1500),
        &Default::default(),
    )
}

#[test]
fn provider_drains_large_stdout_and_stderr_before_waiting() {
    // Each stream exceeds an ordinary pipe's capacity. Both must be drained.
    let block = "x".repeat(192_000);
    let output = capture_script(&format!(
        "#!/bin/sh\nprintf '%s' '{block}'\nprintf '%s' '{block}' >&2\n"
    ))
    .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, block.as_bytes());
    assert_eq!(output.stderr, block.as_bytes());
}

#[test]
fn provider_rejects_output_above_capture_limit() {
    let block = "x".repeat(1024 * 1024 + 1);
    let error = capture_script(&format!("#!/bin/sh\nprintf '%s' '{block}'\n")).unwrap_err();
    assert!(error.to_string().contains("output limit"), "{error}");
}

#[test]
fn expired_nx_cli_cache_refreshes_deep_projects() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("nx.json"), "{}").unwrap();
    let project = dir.path().join("a/b/c/d/e/project.json");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(&project, r#"{"targets":{"build":{}}}"#).unwrap();
    executable(
        &dir.path().join("node_modules/.bin/nx"),
        "#!/bin/sh\ncase \"$2\" in projects) printf '[\"api\"]';; project) /bin/cat a/b/c/d/e/project.json;; esac\n",
    );
    let runtime = empty_runtime();
    assert_eq!(
        list_tasks_in_dir(dir.path(), &runtime).unwrap()[0].name,
        "api:build"
    );
    fs::write(&project, r#"{"targets":{"newtask":{}}}"#).unwrap();
    // Set the real entry's signature to a previous provider epoch instead of
    // sleeping. The next call exercises the normal process-local cache path.
    let root = dir.path().canonicalize().unwrap();
    TASK_CACHE.lock().unwrap().get_mut(&root).unwrap().signature = runtime::discovery_signature_at(
        &root,
        None,
        &runtime,
        SystemTime::now() - Duration::from_secs(60),
    );
    assert_eq!(
        list_tasks_in_dir(dir.path(), &runtime).unwrap()[0].name,
        "api:newtask"
    );
}

#[test]
fn nx_provider_expiration_is_shared_but_static_signatures_do_not_expire() {
    let dir = tempdir().unwrap();
    local_nx(dir.path());
    let runtime = empty_runtime();
    let at = |seconds, sources| {
        runtime::discovery_signature_at(
            dir.path(),
            sources,
            &runtime,
            UNIX_EPOCH + Duration::from_secs(seconds),
        )
    };
    assert_eq!(at(100, None), at(104, None));
    assert_ne!(at(104, None), at(105, None));
    assert_ne!(at(104, Some(&["nx"])), at(105, Some(&["nx"])));
    assert_eq!(at(100, Some(&["turbo"])), at(105, Some(&["turbo"])));
    fs::remove_file(dir.path().join("node_modules/.bin/nx")).unwrap();
    assert_eq!(at(100, None), at(105, None));
}

#[test]
fn metadata_only_never_starts_local_turbo() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("turbo.json"), r#"{"tasks":{"build":{}}}"#).unwrap();
    executable(
        &dir.path().join("node_modules/.bin/turbo"),
        "#!/bin/sh\nprintf ran > provider-ran\nprintf '{\"tasks\":[{\"task\":\"build\"}]}'\n",
    );
    let summary = summarize_tasks_in_dir_metadata_only(dir.path()).unwrap();
    assert!(!dir.path().join("provider-ran").exists());
    assert_eq!(summary.tasks[0].name, "build");
}

#[test]
fn source_filter_skips_invalid_unrelated_static_configs() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("nx.json"), "{}").unwrap();
    fs::write(dir.path().join("project.json"), "invalid JSON").unwrap();
    fs::write(dir.path().join("mise.toml"), "invalid TOML").unwrap();
    fs::write(dir.path().join("turbo.json"), r#"{"tasks":{"build":{}}}"#).unwrap();
    let tasks = list_tasks_in_dir_for_sources(dir.path(), &["turbo"], &empty_runtime()).unwrap();
    assert_eq!(tasks[0].name, "build");
}

#[test]
fn task_source_option_admits_only_selected_provider() {
    let dir = tempdir().unwrap();
    local_nx(dir.path());
    fs::write(dir.path().join("turbo.json"), r#"{"tasks":{"build":{}}}"#).unwrap();
    let mut proxy = crate::test_support::TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        ..Default::default()
    };
    let pid = nix::unistd::getpid();
    let ctx = Context::new_safe(pid, pid, false);
    assert_eq!(
        command(
            &ctx,
            ["task", "--json", "--source", "turbo"]
                .map(str::to_string)
                .to_vec(),
            &mut proxy
        ),
        ExitStatus::ExitedWith(0)
    );
    assert!(!dir.path().join("provider-ran").exists());
}

#[test]
fn provider_accepts_exact_limit_on_both_streams() {
    let block = "x".repeat(1024 * 1024);
    let output = capture_script(&format!(
        "#!/bin/sh\nprintf '%s' '{block}'\nprintf '%s' '{block}' >&2\n"
    ))
    .unwrap();
    assert_eq!(output.stdout.len(), block.len());
    assert_eq!(output.stderr.len(), block.len());
}

#[test]
fn provider_rejects_stderr_over_limit() {
    let block = "x".repeat(1024 * 1024 + 1);
    let error = capture_script(&format!("#!/bin/sh\nprintf '%s' '{block}' >&2\n")).unwrap_err();
    assert!(error.to_string().contains("output limit"), "{error}");
}

#[test]
fn large_mise_json_remains_machine_discovered() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("mise.toml"),
        "[tasks]\nstatic = 'echo static'\n",
    )
    .unwrap();
    let bin = dir.path().join("bin");
    let payload = serde_json::to_string(
        &(0..12000)
            .map(|i| format!("dynamic_{i:05}"))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    executable(
        &bin.join("mise"),
        &format!("#!/bin/sh\nprintf '%s' '{payload}'\n"),
    );
    let runtime = TaskDiscoveryRuntime::new(vec![bin], HashMap::new());
    let tasks = list_tasks_in_dir_for_sources(dir.path(), &["mise"], &runtime).unwrap();
    assert_eq!(tasks.len(), 12000);
    assert!(tasks.iter().any(|t| t.name == "dynamic_11999"));
    assert!(!tasks.iter().any(|t| t.name == "static"));
}

fn wait_for_fifo_reader_disconnect(
    fd: &impl std::os::fd::AsRawFd,
    timeout: Duration,
) -> (bool, nix::libc::c_short) {
    use nix::libc;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let mut event = libc::pollfd {
            fd: fd.as_raw_fd(),
            // Darwin only registers the write filter when an output event is
            // requested. Writable readiness alone is not a disconnect.
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: the borrowed fd remains owned throughout this poll.
        let polled = unsafe { libc::poll(&mut event, 1, 10) };
        if polled < 0 {
            let error = std::io::Error::last_os_error();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::Interrupted,
                "polling FIFO writer: {error}"
            );
        }
        assert_eq!(event.revents & libc::POLLNVAL, 0, "invalid FIFO writer fd");
        if event.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return (true, event.revents);
        }
        if std::time::Instant::now() >= deadline {
            return (false, event.revents);
        }
        // A live reader makes POLLOUT immediately ready. Keep observing it
        // without feeding the FIFO or spinning while waiting for it to close.
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn fifo_disconnect_observer_requires_reader_to_close() {
    use nix::{
        fcntl::{OFlag, open},
        sys::stat::Mode,
        unistd::mkfifo,
    };
    let dir = tempdir().unwrap();
    let gate = dir.path().join("gate");
    mkfifo(&gate, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let reader = open(&gate, OFlag::O_RDONLY | OFlag::O_NONBLOCK, Mode::empty()).unwrap();
    let writer = open(&gate, OFlag::O_WRONLY | OFlag::O_NONBLOCK, Mode::empty()).unwrap();
    assert!(
        !wait_for_fifo_reader_disconnect(&writer, Duration::from_millis(30)).0,
        "a live FIFO reader must not be reported as disconnected"
    );
    drop(reader);
    let (disconnected, revents) = wait_for_fifo_reader_disconnect(&writer, Duration::from_secs(1));
    assert!(
        disconnected,
        "closed FIFO reader was not observed (last_poll_revents={revents:#x})"
    );
}

#[test]
fn provider_timeout_closes_descendant_pipes_and_fifo_readers() {
    use nix::{
        errno::Errno,
        fcntl::{OFlag, open},
        sys::stat::Mode,
        unistd::mkfifo,
    };
    use std::io::Write;
    for parent_exits in [false, true] {
        let dir = tempdir().unwrap();
        let gate = dir.path().join("gate");
        mkfifo(&gate, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let script = dir.path().join("provider");
        // The ready marker is a handshake, not a timing assumption. The
        // descendant holds both output pipes while blocked on the FIFO.
        executable(
            &script,
            &format!(
                "#!/bin/sh\n(exec 3<>gate; printf ready > ready; read line <&3; printf escaped > escaped) &\nwhile [ ! -f ready ]; do :; done\n{}\n",
                if parent_exits { "exit 0" } else { "wait" }
            ),
        );
        let start = std::time::Instant::now();
        let result = command_output_with_timeout(
            &script,
            &[],
            dir.path(),
            Duration::from_millis(200),
            &Default::default(),
        );
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(dir.path().join("ready").exists());
        let mut last_revents = 0;
        let writer = open(&gate, OFlag::O_WRONLY | OFlag::O_NONBLOCK, Mode::empty());
        let disconnected = match writer {
            Err(Errno::ENXIO) => true,
            Ok(fd) => {
                let (gone, revents) = wait_for_fifo_reader_disconnect(&fd, Duration::from_secs(1));
                last_revents = revents;
                if !gone {
                    let mut file = std::fs::File::from(fd);
                    let _ = file.write_all(b"release\n");
                }
                gone
            }
            Err(error) => panic!("opening FIFO writer: {error}"),
        };
        assert!(
            disconnected,
            "a descendant still owns a FIFO reader (parent_exits={parent_exits}, last_poll_revents={last_revents:#x})"
        );
    }
}

#[test]
fn completed_discovery_refreshes_only_provider_epoch() {
    let dir = tempdir().unwrap();
    local_nx(dir.path());
    let runtime = empty_runtime();
    let at = |seconds| {
        runtime::discovery_signature_at(
            dir.path(),
            None,
            &runtime,
            UNIX_EPOCH + Duration::from_secs(seconds),
        )
    };
    let before = at(100);
    let expected_completed = at(110);
    fs::write(
        dir.path().join("package.json"),
        r#"{"scripts":{"newtask":"echo new"}}"#,
    )
    .unwrap();
    let refreshed = before.refresh_provider_epoch_at(UNIX_EPOCH + Duration::from_secs(110));
    assert_eq!(
        refreshed, expected_completed,
        "a slow successful load must not be expired immediately"
    );
    assert_ne!(
        refreshed,
        at(110),
        "refreshing the epoch must not hide concurrent input edits"
    );
}
