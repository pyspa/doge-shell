//! Herdr backend: reports lifecycle state via the `herdr pane report-agent`/
//! `release-agent` CLI, when running inside a Herdr pane.
//!
//! This uses the portable CLI integration Herdr's own docs recommend for a
//! custom agent, not the raw socket API (`HERDR_SOCKET_PATH` is read by
//! [`HerdrEnv::detect`] but otherwise unused - out of scope, see the task's
//! Step 15).

use super::{AgentState, LifecycleReporter};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::debug;
use wait_timeout::ChildExt;

/// Process-visible marker a nested `dsh` checks for. Not a shell setting:
/// see `dsh/src/agent_lifecycle/mod.rs::activate` for how the owning
/// process publishes it to children.
pub(super) const OWNER_ENV: &str = "DSH_HERDR_OWNER_PID";

/// How long a single `herdr` invocation is allowed to run before it's
/// killed. Mirrors the timeout+killpg idiom already used for AI chat hooks
/// (`dsh-builtin/src/chatgpt/hooks/runner.rs`) and the atuin dual-write
/// fire-and-forget calls (`dsh/src/history/command_history.rs`).
const COMMAND_TIMEOUT: Duration = Duration::from_millis(750);

pub(crate) struct HerdrEnv {
    pub(crate) bin_path: PathBuf,
    pub(crate) pane_id: String,
}

impl HerdrEnv {
    /// Reads the *process* environment only, deliberately bypassing this
    /// shell's usual "shell variable, then process environment" resolution
    /// order (`dsh-builtin`'s `resolve_setting`). `HERDR_ENV`/`HERDR_PANE_ID`/
    /// `HERDR_BIN_PATH` are ambient facts about how this process was
    /// launched, not user-configurable dsh settings - the same reasoning
    /// `DSH_HOOK_DEPTH` already uses (`dsh-builtin/src/chatgpt/hooks/config.rs`):
    /// a shell variable must not be able to spoof "running under Herdr" or
    /// suppress it.
    pub(crate) fn detect() -> Option<Self> {
        Self::detect_impl(true)
    }

    /// Same detection, but skips the owner-marker exclusion. For a process
    /// that forked (not exec'd) from a `dsh` that already activated Herdr -
    /// `fork()` only duplicates the calling thread, so the parent's
    /// `HerdrReporter` worker thread simply does not exist in the child even
    /// though its `Arc` does. That child is a continuation of the *same*
    /// session's own lifecycle authority, not a competing shell, so the
    /// marker (which exists to stop a genuinely separate nested `dsh` from
    /// contending for the same pane) must not block it. See
    /// `super::reactivate_after_fork`.
    pub(crate) fn detect_for_forked_child() -> Option<Self> {
        Self::detect_impl(false)
    }

    fn detect_impl(check_owner_marker: bool) -> Option<Self> {
        if std::env::var("HERDR_ENV").ok().as_deref() != Some("1") {
            return None;
        }
        // An ancestor `dsh` in this same pane already claimed lifecycle
        // authority (this marker only ever reaches a child through this
        // shell's own envp construction for spawned processes, so seeing it
        // here means a `dsh` ancestor set it, not an unrelated tool).
        if check_owner_marker && std::env::var_os(OWNER_ENV).is_some() {
            return None;
        }
        let pane_id = non_empty_env("HERDR_PANE_ID")?;
        let bin_path = non_empty_env("HERDR_BIN_PATH")?;
        Some(Self {
            bin_path: PathBuf::from(bin_path),
            pane_id,
        })
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

enum Job {
    Report { state: AgentState, seq: u64 },
    Release { seq: u64 },
}

/// Herdr-backed [`LifecycleReporter`]. Every `report`/`release` call
/// computes its `argv` and hands it to a dedicated worker thread over an
/// `mpsc` channel; the worker processes jobs strictly in send order, which
/// is what actually guarantees delivery order (not `--seq` alone - see this
/// module's parent doc comment).
pub struct HerdrReporter {
    tx: Mutex<Option<mpsc::Sender<Job>>>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    /// The state the worker most recently confirmed a successful `herdr`
    /// invocation for. Read by `is_stale` for the manager's periodic
    /// reconciliation.
    delivered: Arc<Mutex<Option<AgentState>>>,
}

impl HerdrReporter {
    pub(crate) fn spawn(env: HerdrEnv, source: &'static str, agent: &'static str) -> Arc<Self> {
        let (tx, rx) = mpsc::channel::<Job>();
        let delivered = Arc::new(Mutex::new(None));
        let worker_delivered = delivered.clone();
        let handle = thread::spawn(move || {
            worker_loop(
                rx,
                env.bin_path,
                env.pane_id,
                source,
                agent,
                worker_delivered,
            );
        });
        Arc::new(Self {
            tx: Mutex::new(Some(tx)),
            worker: Mutex::new(Some(handle)),
            delivered,
        })
    }

    fn send(&self, job: Job) {
        if let Some(tx) = lock(&self.tx).as_ref() {
            // The receiver only disappears once `shutdown` has already
            // closed the channel; a send failing after that point is
            // expected and not worth logging.
            let _ = tx.send(job);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl LifecycleReporter for HerdrReporter {
    fn report(&self, state: &AgentState, seq: u64) {
        self.send(Job::Report {
            state: state.clone(),
            seq,
        });
    }

    fn release(&self, seq: u64) {
        self.send(Job::Release { seq });
    }

    fn shutdown(&self, timeout: Duration) {
        // Dropping the sender ends the worker's `for job in rx` loop once
        // every already-queued job (including the `release` just sent) has
        // been processed.
        lock(&self.tx).take();
        if let Some(handle) = lock(&self.worker).take() {
            join_with_timeout(handle, timeout);
        }
    }

    fn is_stale(&self, desired: &AgentState) -> bool {
        lock(&self.delivered).as_ref() != Some(desired)
    }

    fn is_active(&self) -> bool {
        true
    }
}

/// `JoinHandle::join` has no timeout of its own. This shell's exit path is
/// the only caller and is allowed to block briefly here (the process is
/// about to end either way), but must never hang indefinitely on a wedged
/// `herdr` process - hence a bounded poll instead of an unbounded join. A
/// handle that doesn't finish in time is simply left running; the worker's
/// own per-command `COMMAND_TIMEOUT` still bounds it from the inside.
fn join_with_timeout(handle: thread::JoinHandle<()>, timeout: Duration) {
    let start = Instant::now();
    while !handle.is_finished() {
        if start.elapsed() >= timeout {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = handle.join();
}

fn worker_loop(
    rx: mpsc::Receiver<Job>,
    bin_path: PathBuf,
    pane_id: String,
    source: &str,
    agent: &str,
    delivered: Arc<Mutex<Option<AgentState>>>,
) {
    for job in rx {
        let (args, reported_state) = match &job {
            Job::Report { state, seq } => (
                build_report_args(&pane_id, source, agent, state, *seq),
                Some(state.clone()),
            ),
            Job::Release { seq } => (build_release_args(&pane_id, source, agent, *seq), None),
        };
        if run_herdr(&bin_path, &args) && reported_state.is_some() {
            *lock(&delivered) = reported_state;
        }
    }
}

/// Runs one `herdr` invocation to completion or gives up. Never leaves a
/// zombie behind: a command that outlives `COMMAND_TIMEOUT` has its whole
/// process group killed (`.process_group(0)` below makes `herdr` its own
/// group leader), the same `killpg` idiom
/// `dsh-builtin/src/chatgpt/tool/execute.rs::kill_process_group` uses for the
/// AI `execute` tool and AI chat hooks - a plain `child.kill()` would only
/// reach `herdr` itself and orphan anything it spawned. Every failure mode
/// (missing binary, non-zero exit, timeout) is swallowed here and only
/// logged at `debug!` - Herdr reporting failure must never surface to the
/// user or fail the agent turn that triggered it (Step 9).
fn run_herdr(bin_path: &Path, args: &[String]) -> bool {
    let mut child = match Command::new(bin_path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            debug!("herdr lifecycle: failed to spawn {bin_path:?}: {err}");
            return false;
        }
    };
    match child.wait_timeout(COMMAND_TIMEOUT) {
        Ok(Some(status)) => {
            if !status.success() {
                debug!("herdr lifecycle: {bin_path:?} {args:?} exited with {status}");
            }
            status.success()
        }
        Ok(None) => {
            kill_process_group(&child);
            let _ = child.wait();
            debug!("herdr lifecycle: {bin_path:?} {args:?} timed out after {COMMAND_TIMEOUT:?}");
            false
        }
        Err(err) => {
            debug!("herdr lifecycle: wait failed for {bin_path:?}: {err}");
            false
        }
    }
}

fn kill_process_group(child: &std::process::Child) {
    let Ok(pid) = i32::try_from(child.id()) else {
        return;
    };
    // `.process_group(0)` above made the child its own group leader, so
    // pgid == pid.
    let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), nix::sys::signal::SIGKILL);
}

fn state_flag(state: &AgentState) -> &'static str {
    match state {
        AgentState::Idle => "idle",
        AgentState::Working => "working",
        AgentState::Blocked(_) => "blocked",
    }
}

/// The `pane <subcommand> <pane_id> --source <source> --agent <agent>`
/// prefix every `herdr pane` invocation this module makes shares.
fn pane_command_prefix(subcommand: &str, pane_id: &str, source: &str, agent: &str) -> Vec<String> {
    vec![
        "pane".to_string(),
        subcommand.to_string(),
        pane_id.to_string(),
        "--source".to_string(),
        source.to_string(),
        "--agent".to_string(),
        agent.to_string(),
    ]
}

fn build_report_args(
    pane_id: &str,
    source: &str,
    agent: &str,
    state: &AgentState,
    seq: u64,
) -> Vec<String> {
    let mut args = pane_command_prefix("report-agent", pane_id, source, agent);
    args.push("--state".to_string());
    args.push(state_flag(state).to_string());
    if let AgentState::Blocked(reason) = state {
        args.push("--message".to_string());
        args.push(reason.clone());
    }
    args.push("--seq".to_string());
    args.push(seq.to_string());
    args
}

fn build_release_args(pane_id: &str, source: &str, agent: &str, seq: u64) -> Vec<String> {
    let mut args = pane_command_prefix("release-agent", pane_id, source, agent);
    args.push("--seq".to_string());
    args.push(seq.to_string());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear_all() {
        // SAFETY: single-threaded under `crate::test_env_lock()`.
        unsafe {
            for key in ["HERDR_ENV", "HERDR_PANE_ID", "HERDR_BIN_PATH", OWNER_ENV] {
                std::env::remove_var(key);
            }
        }
    }

    #[test]
    fn detects_when_all_required_vars_present() {
        let _guard = crate::test_env_lock();
        clear_all();
        // SAFETY: single-threaded under `crate::test_env_lock()`.
        unsafe {
            std::env::set_var("HERDR_ENV", "1");
            std::env::set_var("HERDR_PANE_ID", "w1:p1");
            std::env::set_var("HERDR_BIN_PATH", "/opt/herdr-fixture/herdr");
        }
        let env = HerdrEnv::detect().expect("should detect");
        assert_eq!(env.pane_id, "w1:p1");
        assert_eq!(env.bin_path, PathBuf::from("/opt/herdr-fixture/herdr"));
        clear_all();
    }

    #[test]
    fn missing_herdr_env_disables() {
        let _guard = crate::test_env_lock();
        clear_all();
        // SAFETY: single-threaded under `crate::test_env_lock()`.
        unsafe {
            std::env::set_var("HERDR_PANE_ID", "w1:p1");
            std::env::set_var("HERDR_BIN_PATH", "/opt/herdr-fixture/herdr");
        }
        assert!(HerdrEnv::detect().is_none());
        clear_all();
    }

    #[test]
    fn non_one_herdr_env_disables() {
        let _guard = crate::test_env_lock();
        clear_all();
        // SAFETY: single-threaded under `crate::test_env_lock()`.
        unsafe {
            std::env::set_var("HERDR_ENV", "true");
            std::env::set_var("HERDR_PANE_ID", "w1:p1");
            std::env::set_var("HERDR_BIN_PATH", "/opt/herdr-fixture/herdr");
        }
        assert!(HerdrEnv::detect().is_none());
        clear_all();
    }

    #[test]
    fn missing_pane_id_disables() {
        let _guard = crate::test_env_lock();
        clear_all();
        // SAFETY: single-threaded under `crate::test_env_lock()`.
        unsafe {
            std::env::set_var("HERDR_ENV", "1");
            std::env::set_var("HERDR_BIN_PATH", "/opt/herdr-fixture/herdr");
        }
        assert!(HerdrEnv::detect().is_none());
        clear_all();
    }

    #[test]
    fn missing_bin_path_disables() {
        let _guard = crate::test_env_lock();
        clear_all();
        // SAFETY: single-threaded under `crate::test_env_lock()`.
        unsafe {
            std::env::set_var("HERDR_ENV", "1");
            std::env::set_var("HERDR_PANE_ID", "w1:p1");
        }
        assert!(HerdrEnv::detect().is_none());
        clear_all();
    }

    #[test]
    fn owner_marker_already_set_disables() {
        let _guard = crate::test_env_lock();
        clear_all();
        // SAFETY: single-threaded under `crate::test_env_lock()`.
        unsafe {
            std::env::set_var("HERDR_ENV", "1");
            std::env::set_var("HERDR_PANE_ID", "w1:p1");
            std::env::set_var("HERDR_BIN_PATH", "/opt/herdr-fixture/herdr");
            std::env::set_var(OWNER_ENV, "1234");
        }
        assert!(HerdrEnv::detect().is_none());
        clear_all();
    }

    #[test]
    fn report_args_for_idle() {
        let args = build_report_args("w1:p1", "custom:doge-shell", "dsh", &AgentState::Idle, 42);
        assert_eq!(
            args,
            vec![
                "pane",
                "report-agent",
                "w1:p1",
                "--source",
                "custom:doge-shell",
                "--agent",
                "dsh",
                "--state",
                "idle",
                "--seq",
                "42",
            ]
        );
    }

    #[test]
    fn report_args_for_working() {
        let args = build_report_args("w1:p1", "custom:doge-shell", "dsh", &AgentState::Working, 1);
        assert!(args.iter().any(|a| a == "working"));
        assert!(!args.iter().any(|a| a == "--message"));
    }

    #[test]
    fn report_args_for_blocked_include_message() {
        let args = build_report_args(
            "w1:p1",
            "custom:doge-shell",
            "dsh",
            &AgentState::Blocked("needs approval to delete files".to_string()),
            7,
        );
        assert!(args.iter().any(|a| a == "blocked"));
        let message_pos = args
            .iter()
            .position(|a| a == "--message")
            .expect("--message present");
        assert_eq!(args[message_pos + 1], "needs approval to delete files");
    }

    #[test]
    fn release_args_omit_state_and_message() {
        let args = build_release_args("w1:p1", "custom:doge-shell", "dsh", 9);
        assert_eq!(args[0], "pane");
        assert_eq!(args[1], "release-agent");
        assert_eq!(args[2], "w1:p1");
        assert!(!args.iter().any(|a| a == "--state"));
        assert!(!args.iter().any(|a| a == "--message"));
        assert!(args.iter().any(|a| a == "--seq"));
    }

    #[test]
    fn detect_for_forked_child_ignores_the_owner_marker() {
        let _guard = crate::test_env_lock();
        clear_all();
        // SAFETY: single-threaded under `crate::test_env_lock()`.
        unsafe {
            std::env::set_var("HERDR_ENV", "1");
            std::env::set_var("HERDR_PANE_ID", "w1:p1");
            std::env::set_var("HERDR_BIN_PATH", "/opt/herdr-fixture/herdr");
            // As if a parent `dsh` in the same pane already activated Herdr -
            // exactly the situation a forked child of that same process is
            // in, and must not be excluded by.
            std::env::set_var(OWNER_ENV, "1234");
        }
        assert!(
            HerdrEnv::detect().is_none(),
            "the plain detector still excludes it"
        );
        assert!(
            HerdrEnv::detect_for_forked_child().is_some(),
            "the forked-child detector must not"
        );
        clear_all();
    }

    #[test]
    fn non_empty_env_trims_before_returning() {
        let _guard = crate::test_env_lock();
        clear_all();
        // SAFETY: single-threaded under `crate::test_env_lock()`.
        unsafe {
            std::env::set_var("HERDR_PANE_ID", "  w1:p1 \n");
        }
        assert_eq!(non_empty_env("HERDR_PANE_ID").as_deref(), Some("w1:p1"));
        clear_all();
    }
}
