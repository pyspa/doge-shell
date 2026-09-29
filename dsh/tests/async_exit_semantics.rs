//! Parent-process exit semantics for explicit background jobs.
//!
//! The pipe-capturing harnesses (`common::run_command`, the TOML contract
//! runner) cannot prove that the `dogesh` process itself exits before a
//! long-running background child: a detached helper inherits the pipe write
//! ends, so `wait_with_output` blocks past the parent's exit. These tests
//! therefore spawn `dogesh` with `Stdio::null` or regular-file stdio and
//! prove, per case:
//!
//! 1. the parent process exits within a bound far below the child's sleep,
//! 2. the background helper is still alive afterwards (`kill(pid, 0)`),
//! 3. late side effects (marker file, regular-file stdout/stderr) land
//!    after the parent's exit,
//! 4. every stray is group-killed on all paths, including panics (RAII).

mod common;

use nix::sys::signal::{Signal, kill, killpg};
use nix::sys::wait::{WaitPidFlag, waitpid};
use nix::unistd::{Pid, getpgrp, getpid};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use wait_timeout::ChildExt as _;

/// A live `dogesh` child spawned for exit observation, with its isolated
/// dirs. The stdio is never piped: either null or a regular file, so no
/// inherited write end can mask the parent's own exit.
struct ObservedParent {
    child: Option<Child>,
    pid: Pid,
    workdir: PathBuf,
    _temp: TempDir,
}

impl ObservedParent {
    fn assert_running(&mut self, context: &str) {
        let child = self.child.as_mut().expect("parent already waited");
        let status = child.try_wait().expect("failed to inspect dogesh parent");
        assert!(
            status.is_none(),
            "{context}: dogesh parent exited unexpectedly: {status:?}"
        );
    }

    fn wait_exit(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let child = self.child.as_mut().expect("parent already waited");
        match child
            .wait_timeout(timeout)
            .expect("failed while waiting for dogesh parent")
        {
            Some(status) => status,
            None => {
                let _ = killpg(Pid::from_raw(self.pid.as_raw()), Signal::SIGKILL);
                let _ = child.kill();
                panic!(
                    "dogesh parent {pid} did not exit within {timeout:?}",
                    pid = self.pid
                );
            }
        }
    }
}

impl Drop for ObservedParent {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = killpg(Pid::from_raw(self.pid.as_raw()), Signal::SIGKILL);
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A background helper that outlived its parent shell. Cleanup is group
/// `SIGKILL` plus a bounded disappearance poll; `Drop` repeats it
/// best-effort so panics never leak processes into CI.
struct SurvivingChild {
    pid: Pid,
    pgid: Pid,
    cleaned: bool,
}

impl SurvivingChild {
    fn new(pid: Pid, pgid: Pid) -> Self {
        assert!(pid.as_raw() > 0, "helper pid must be positive");
        assert!(pgid.as_raw() > 0, "helper pgid must be positive");
        assert_ne!(pid, getpid(), "must never target the test process");
        assert_ne!(pgid, getpgrp(), "must never target the test runner group");
        Self {
            pid,
            pgid,
            cleaned: false,
        }
    }

    fn assert_alive(&self) {
        assert!(
            kill(self.pid, None).is_ok(),
            "background helper {} must be alive after parent exit",
            self.pid
        );
    }

    fn cleanup(&mut self) {
        if self.cleaned {
            return;
        }
        self.cleaned = true;
        let _ = killpg(self.pgid, Signal::SIGKILL);
        let _ = kill(self.pid, Signal::SIGKILL);
        let deadline = Instant::now() + Duration::from_secs(10);
        while kill(self.pid, None).is_ok() {
            assert!(
                Instant::now() < deadline,
                "background helper {} never died after group SIGKILL",
                self.pid
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // Reap when it is still our child (unit-level spawns); reparented
        // helpers report ECHILD here, which is success-equivalent.
        let _ = waitpid(self.pid, Some(WaitPidFlag::WNOHANG));
    }
}

impl Drop for SurvivingChild {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = killpg(self.pgid, Signal::SIGKILL);
            let _ = kill(self.pid, Signal::SIGKILL);
        }
    }
}

/// Spawn `dogesh -c <script>` with caller-chosen stdio. The caller must hold
/// `common::serial_guard()`; this spawner takes no lock itself.
fn spawn_parent(script: &str, stdout: Stdio, stderr: Stdio) -> ObservedParent {
    let temp = TempDir::new().expect("isolated dsh test directory");
    let workdir = temp.path().join("work");
    std::fs::create_dir_all(&workdir).expect("isolated dsh cwd");
    let child = Command::new(env!("CARGO_BIN_EXE_dogesh"))
        .arg("-c")
        .arg(script)
        .current_dir(&workdir)
        .env("HOME", temp.path().join("home"))
        .env("XDG_STATE_HOME", temp.path().join("state"))
        .env("XDG_DATA_HOME", temp.path().join("data"))
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0)
        .spawn()
        .expect("spawn dogesh parent");
    let pid = Pid::from_raw(child.id() as i32);
    assert_ne!(pid, getpid());
    ObservedParent {
        child: Some(child),
        pid,
        workdir,
        _temp: temp,
    }
}

/// Parent pid of another process, via `ps` (portable across Linux/macOS;
/// no `/proc` dependency). `None` when the process is already gone.
fn parent_pid_of(pid: Pid) -> Option<Pid> {
    let output = Command::new("ps")
        .arg("-o")
        .arg("ppid=")
        .arg("-p")
        .arg(pid.as_raw().to_string())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<i32>()
        .ok()?;
    (raw > 0).then(|| Pid::from_raw(raw))
}

/// Wait until `pid` is still alive but parented to someone other than
/// `original_parent`. Returns the adopting parent. Liveness authority is
/// `kill(pid, 0)`; a transient `None` from `ps` never counts as death.
fn wait_for_parent_change_while_alive(pid: Pid, original_parent: Pid, timeout: Duration) -> Pid {
    let deadline = Instant::now() + timeout;
    loop {
        assert!(
            kill(pid, None).is_ok(),
            "nested helper {pid} died before its subshell parent released it"
        );
        if let Some(current_parent) = parent_pid_of(pid)
            && current_parent != original_parent
        {
            return current_parent;
        }
        assert!(
            Instant::now() < deadline,
            "nested helper {pid} remained parented to {original_parent}; \
             subshell helper did not exit within {timeout:?} \
             (or the original parent was already gone before observation)"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Record the nested helper's parent while the subshell helper is
/// deterministically held alive by the FIFO handshake (it is blocked in
/// `head -n 1 sync.fifo`, which only the test can release). A single `ps`
/// read is authoritative: no inter-read sleep is needed because the hold is
/// synchronized, not inferred from elapsed time. The extra checks prove the
/// recorded pid really is the live subshell helper — a running child of the
/// top shell — and not an already-adopted parent observed through
/// scheduling luck.
fn record_original_parent_while_held(nested: Pid, top: Pid) -> Pid {
    let original = parent_pid_of(nested)
        .expect("nested helper must have a parent while the subshell is held in the handshake");
    assert_ne!(
        original, nested,
        "nested helper {nested} cannot be its own parent"
    );
    assert!(
        original.as_raw() > 0,
        "original parent pid must be positive"
    );
    assert!(
        kill(original, None).is_ok(),
        "original subshell parent {original} must still be alive: \
         the release has not been sent yet"
    );
    assert_eq!(
        parent_pid_of(original),
        Some(top),
        "original parent {original} must still be a child of the top shell {top}; \
         otherwise the subshell already exited before observation"
    );
    original
}

/// Explicitly release a subshell helper blocked in `head -n 1 sync.fifo`.
///
/// The FIFO is opened `O_RDWR`, which never blocks on Linux or macOS
/// regardless of whether the reader has arrived yet, so endpoint-open
/// ordering cannot deadlock. One line suffices: `head -n 1` exits on the
/// first line without needing EOF. The returned handle must stay alive until
/// the subshell's exit has been observed: closing it before `head` opens the
/// FIFO would discard the buffered line and strand the subshell (lost
/// wakeup). Dropping it afterwards only closes an already-consumed endpoint.
fn release_subshell_via_fifo(fifo: &std::path::Path) -> std::fs::File {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fifo.exists() {
        assert!(
            Instant::now() < deadline,
            "sync fifo {} never appeared",
            fifo.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut releaser = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fifo)
        .expect("sync fifo must open O_RDWR without blocking");
    use std::io::Write as _;
    releaser
        .write_all(b"go\n")
        .expect("release line must write to the sync fifo");
    releaser.flush().expect("release line must flush");
    releaser
}

/// Process group of another process, via `ps` (portable across
/// Linux/macOS; no `/proc` dependency). `None` when already gone.
fn pgid_of(pid: Pid) -> Option<i32> {
    let output = Command::new("ps")
        .arg("-o")
        .arg("pgid=")
        .arg("-p")
        .arg(pid.as_raw().to_string())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<i32>()
        .ok()
}

/// Assert `pid` stays alive for the whole window: repeated `kill(pid, 0)`
/// probes, never one sleep-then-check, so a mid-window death cannot hide
/// behind a live probe at either end.
fn assert_survives_for(pid: Pid, duration: Duration) {
    let deadline = Instant::now() + duration;
    loop {
        assert!(
            kill(pid, None).is_ok(),
            "process {pid} died during the survival window"
        );
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Start `script` (an async list recording `$!` in `async.pid`) without job
/// control and return its surviving helper after the parent shell exited.
/// The helper leads its own process group by contract, so group signals
/// reach the helper and its inner commands.
fn spawn_detached_sleep_helper(script: &str) -> (ObservedParent, SurvivingChild) {
    let mut parent = spawn_parent(script, Stdio::null(), Stdio::null());
    let status = parent.wait_exit(Duration::from_secs(15));
    assert!(
        status.success(),
        "parent launch must report 0, got {status:?}"
    );
    let helper = read_helper_pid(&parent.workdir, "async.pid");
    assert_eq!(
        pgid_of(helper),
        Some(helper.as_raw()),
        "async helper {helper} must lead its own process group"
    );
    let survivor = SurvivingChild::new(helper, helper);
    survivor.assert_alive();
    (parent, survivor)
}

fn read_helper_pid(workdir: &std::path::Path, name: &str) -> Pid {
    let path = workdir.join(name);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(raw) = text.trim().parse::<i32>()
            && raw > 0
        {
            let pid = Pid::from_raw(raw);
            assert_ne!(pid, getpid());
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "helper pid file {name} never appeared"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Wait until a `sleep` process exists in `pgid`. Nested helpers take time
/// to spawn, and the group signal below must demonstrably reach the full
/// tree — not land before the inner levels exist. `pgrep -g`/`-x` exist on
/// both Linux (procps) and macOS; no `/proc` dependency.
fn poll_group_sleep(pgid: Pid, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let found = Command::new("pgrep")
            .arg("-g")
            .arg(pgid.as_raw().to_string())
            .arg("-x")
            .arg("sleep")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if found {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no sleep process appeared in group {pgid}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Wait until no `sleep` process remains in `pgid`. Group SIGKILL reaches
/// every group member at once, but an orphaned survivor keeps its pgid
/// across reparenting, so a leaked `sleep` is still caught here by group.
/// `pgrep -g`/`-x` exist on both Linux (procps) and macOS; no `/proc`
/// dependency.
fn poll_no_group_sleep(pgid: Pid, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let found = Command::new("pgrep")
            .arg("-g")
            .arg(pgid.as_raw().to_string())
            .arg("-x")
            .arg("sleep")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !found {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "a sleep process leaked in group {pgid} after cleanup"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn poll_file_contains(path: &std::path::Path, needle: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && text.contains(needle)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "file {} never contained {needle:?}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn sleep_background_parent_exits_promptly_and_child_survives() {
    let _guard = common::serial_guard();
    let mut parent = spawn_parent(
        "sleep 30 & echo $! > async.pid",
        Stdio::null(),
        Stdio::null(),
    );
    let status = parent.wait_exit(Duration::from_secs(15));
    assert!(
        status.success(),
        "parent launch must report 0, got {status:?}"
    );
    let helper = read_helper_pid(&parent.workdir, "async.pid");
    let mut survivor = SurvivingChild::new(helper, helper);
    survivor.assert_alive();
    survivor.cleanup();
}

#[test]
fn foreground_final_status_survives_detach() {
    let _guard = common::serial_guard();
    let mut ok = spawn_parent(
        "sleep 30 & echo $! > async.pid",
        Stdio::null(),
        Stdio::null(),
    );
    assert!(ok.wait_exit(Duration::from_secs(15)).success());
    let helper = read_helper_pid(&ok.workdir, "async.pid");
    let mut survivor = SurvivingChild::new(helper, helper);
    survivor.assert_alive();
    survivor.cleanup();

    let mut failed = spawn_parent(
        "sleep 30 & echo $! > async.pid; false",
        Stdio::null(),
        Stdio::null(),
    );
    let status = failed.wait_exit(Duration::from_secs(15));
    assert_eq!(
        status.code(),
        Some(1),
        "detach must not rewrite the foreground status"
    );
    let helper = read_helper_pid(&failed.workdir, "async.pid");
    let mut survivor = SurvivingChild::new(helper, helper);
    survivor.assert_alive();
    survivor.cleanup();
}

#[test]
fn late_file_side_effect_lands_after_parent_exit() {
    let _guard = common::serial_guard();
    let mut parent = spawn_parent(
        "sleep 1 && printf done > marker & echo $! > async.pid",
        Stdio::null(),
        Stdio::null(),
    );
    let status = parent.wait_exit(Duration::from_secs(15));
    assert!(status.success());
    let helper = read_helper_pid(&parent.workdir, "async.pid");
    let mut survivor = SurvivingChild::new(helper, helper);
    survivor.assert_alive();
    // The parent is gone, yet the helper body continues: the marker can
    // only appear after the parent's exit was already observed above.
    poll_file_contains(
        &parent.workdir.join("marker"),
        "done",
        Duration::from_secs(15),
    );
    survivor.cleanup();
}

#[test]
fn late_stdout_to_regular_file_survives_parent_exit() {
    let _guard = common::serial_guard();
    let out_path = std::env::temp_dir().join(format!("dsh-async-out-{}.txt", std::process::id()));
    let out_file = std::fs::File::create(&out_path).expect("create stdout file");
    let mut parent = spawn_parent(
        "sleep 1 && printf LATE-OUT & echo $! > async.pid",
        Stdio::from(out_file),
        Stdio::null(),
    );
    let status = parent.wait_exit(Duration::from_secs(15));
    assert!(status.success());
    let helper = read_helper_pid(&parent.workdir, "async.pid");
    let mut survivor = SurvivingChild::new(helper, helper);
    survivor.assert_alive();
    poll_file_contains(&out_path, "LATE-OUT", Duration::from_secs(15));
    survivor.cleanup();
    std::fs::remove_file(&out_path).ok();
}

#[test]
fn late_stderr_to_regular_file_survives_parent_exit() {
    let _guard = common::serial_guard();
    let err_path = std::env::temp_dir().join(format!("dsh-async-err-{}.txt", std::process::id()));
    let err_file = std::fs::File::create(&err_path).expect("create stderr file");
    let mut parent = spawn_parent(
        "sleep 1 && printf LATE-ERR >&2 & echo $! > async.pid",
        Stdio::null(),
        Stdio::from(err_file),
    );
    let status = parent.wait_exit(Duration::from_secs(15));
    assert!(status.success());
    let helper = read_helper_pid(&parent.workdir, "async.pid");
    let mut survivor = SurvivingChild::new(helper, helper);
    survivor.assert_alive();
    poll_file_contains(&err_path, "LATE-ERR", Duration::from_secs(15));
    survivor.cleanup();
    std::fs::remove_file(&err_path).ok();
}

#[test]
fn nested_async_child_survives_subshell_helper_exit() {
    let _guard = common::serial_guard();
    let mut parent = spawn_parent(
        "( sleep 30 & echo $! > nested.pid; mkfifo sync.fifo; echo ready > subshell.ready; head -n 1 sync.fifo > /dev/null )",
        Stdio::null(),
        Stdio::null(),
    );
    // Handshake, not wall-clock: the subshell publishes the nested pid, then
    // blocks in `head` on the FIFO until this test sends one line. The
    // subshell therefore cannot exit before the explicit release below, so
    // the original-parent read is synchronized rather than inferred from a
    // fixed observation window. The nested helper inherits the
    // subshell's capture pipe, so the top shell's EOF wait legitimately
    // lasts until the nested child goes away (pipe lifetime, not process
    // waiting). The contract is that the nested async child outlives the
    // subshell helper. The identity of the process that adopts the orphan
    // is deliberately not part of this test: POSIX does not require PID 1,
    // and Linux child subreapers may become the new parent.
    let nested = read_helper_pid(&parent.workdir, "nested.pid");
    poll_file_contains(
        &parent.workdir.join("subshell.ready"),
        "ready",
        Duration::from_secs(10),
    );
    let mut survivor = SurvivingChild::new(nested, nested);
    survivor.assert_alive();
    let original_parent = record_original_parent_while_held(nested, parent.pid);
    parent.assert_running(
        "top shell must remain blocked while the subshell is held in the handshake",
    );
    // Explicit test-controlled release. The held-open FIFO handle keeps the
    // buffered line alive until `head` consumes it (see the release helper).
    let release = release_subshell_via_fifo(&parent.workdir.join("sync.fifo"));
    let adopted_by =
        wait_for_parent_change_while_alive(nested, original_parent, Duration::from_secs(10));
    assert_ne!(
        adopted_by, original_parent,
        "nested helper {nested} must leave its original subshell parent"
    );
    drop(release);
    survivor.assert_alive();
    parent.assert_running(
        "top shell must remain blocked until the nested capture writer is released",
    );
    // Releasing the nested child closes the last capture-pipe write end:
    // the top shell then EOFs and exits 0 without any further wait.
    // Observe the group while the helper is known alive so the leak guard
    // below checks the real group even if group assumptions ever change.
    let nested_group = Pid::from_raw(pgid_of(nested).unwrap_or(nested.as_raw()));
    survivor.cleanup();
    poll_no_group_sleep(nested_group, Duration::from_secs(10));
    let status = parent.wait_exit(Duration::from_secs(15));
    assert!(status.success(), "parent must exit 0 after nested cleanup");
}

#[test]
fn nested_async_child_survives_outer_async_helper_exit() {
    let _guard = common::serial_guard();
    let mut parent = spawn_parent(
        "( sleep 30 & echo $! > nested.pid ) & echo $! > outer.pid",
        Stdio::null(),
        Stdio::null(),
    );
    let status = parent.wait_exit(Duration::from_secs(15));
    assert!(status.success());
    let outer = read_helper_pid(&parent.workdir, "outer.pid");
    let nested = read_helper_pid(&parent.workdir, "nested.pid");
    // The nested helper leads its own process group (it does not join the
    // outer helper's group: the exec'd `sleep` is the helper's child in the
    // helper's group). Group-kill the nested helper's own observed group so
    // the `sleep` child dies with its helper instead of leaking as a
    // reparented orphan; killing the outer group would miss it.
    let nested_pgid = pgid_of(nested)
        .unwrap_or_else(|| panic!("nested helper {nested} must have a process group for cleanup"));
    // `outer.pid` is still awaited above so the outer helper is known to
    // have spawned; only its pid is unneeded for cleanup targeting.
    let _ = outer;
    let mut survivor = SurvivingChild::new(nested, Pid::from_raw(nested_pgid));
    survivor.assert_alive();
    survivor.cleanup();
    // Guard the observed group, not just `survivor.pgid`, so a wrong
    // cleanup target cannot make this check vacuous.
    poll_no_group_sleep(Pid::from_raw(nested_pgid), Duration::from_secs(10));
}

/// Without job control an async AND-OR list starts with SIGINT ignored
/// (POSIX.1-2024): a group SIGINT must kill neither the helper nor the
/// inner command. Either death would end the helper's foreground plan and
/// take the helper down with it, so helper survival proves both.
#[test]
fn non_job_control_async_group_ignores_sigint() {
    let _guard = common::serial_guard();
    let (_parent, mut survivor) = spawn_detached_sleep_helper("sleep 30 & echo $! > async.pid");
    killpg(survivor.pgid, Signal::SIGINT).expect("group SIGINT must deliver");
    assert_survives_for(survivor.pid, Duration::from_secs(2));
    survivor.cleanup();
}

/// Same contract for SIGQUIT, kept as its own test so a failure names the
/// broken disposition instead of hiding inside a combined case.
#[test]
fn non_job_control_async_group_ignores_sigquit() {
    let _guard = common::serial_guard();
    let (_parent, mut survivor) = spawn_detached_sleep_helper("sleep 30 & echo $! > async.pid");
    killpg(survivor.pgid, Signal::SIGQUIT).expect("group SIGQUIT must deliver");
    assert_survives_for(survivor.pid, Duration::from_secs(2));
    survivor.cleanup();
}

/// Opposite case: SIGTERM must still terminate the group. This proves the
/// group targeting, signal delivery, and liveness observation above really
/// work — and catches an implementation that blocks or ignores every
/// signal instead of just SIGINT/SIGQUIT.
#[test]
fn non_job_control_async_group_does_not_ignore_sigterm() {
    let _guard = common::serial_guard();
    let (_parent, mut survivor) = spawn_detached_sleep_helper("sleep 30 & echo $! > async.pid");
    killpg(survivor.pgid, Signal::SIGTERM).expect("group SIGTERM must deliver");
    let deadline = Instant::now() + Duration::from_secs(10);
    while kill(survivor.pid, None).is_ok() {
        assert!(
            Instant::now() < deadline,
            "async helper {} survived group SIGTERM",
            survivor.pid
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    survivor.cleanup();
}

/// One substitution level inside the async list: the `Normal`
/// command-substitution helper preserves the inherited ignore, and the
/// external `sleep` beneath it inherits in turn. Group SIGINT must kill
/// neither.
#[test]
fn non_job_control_nested_substitution_group_ignores_sigint() {
    let _guard = common::serial_guard();
    let (_parent, mut survivor) =
        spawn_detached_sleep_helper("echo $(sleep 30) & echo $! > async.pid");
    poll_group_sleep(survivor.pgid, Duration::from_secs(10));
    killpg(survivor.pgid, Signal::SIGINT).expect("group SIGINT must deliver");
    assert_survives_for(survivor.pid, Duration::from_secs(2));
    survivor.cleanup();
}

/// Two substitution levels: preservation must be transitive. The outer
/// `Normal` helper carries no policy of its own, yet the inner helper and
/// the `sleep` beneath it must still inherit the async ignore — any level
/// reverting to default would cascade into the helper's exit.
#[test]
fn non_job_control_doubly_nested_substitution_group_ignores_sigint() {
    let _guard = common::serial_guard();
    let (_parent, mut survivor) =
        spawn_detached_sleep_helper("echo $(echo $(sleep 30)) & echo $! > async.pid");
    poll_group_sleep(survivor.pgid, Duration::from_secs(10));
    killpg(survivor.pgid, Signal::SIGINT).expect("group SIGINT must deliver");
    assert_survives_for(survivor.pid, Duration::from_secs(2));
    survivor.cleanup();
}
