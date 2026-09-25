pub mod authorize;
pub mod dry_materialize;
pub mod eval;
pub mod hooks;
pub mod job;
pub mod job_exit;
pub mod job_ledger;
pub mod materialize;
pub mod no_command;
pub mod parse;
pub mod pipeline_isolation;
pub mod plan;
pub mod process_substitution;
pub mod struct_pipe;
pub mod substitution;
pub mod word_expand;

use crate::environment::Environment;
use crate::history::{FrecencyHistory, HistoryMetadata};
use crate::lisp;
use crate::process::Job;
use anyhow::Result;
use dsh_types::notebook::NotebookSession;
use dsh_types::{Context, ExitStatus};
use libc::{STDIN_FILENO, c_int};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use nix::unistd::{Pid, getpgrp, getpid};
use parking_lot::Mutex as ParkingMutex;
use parking_lot::RwLock;
use std::collections::VecDeque;
use std::io::Write;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::{cell::RefCell, rc::Rc};
use tracing::{debug, warn};

pub const APP_NAME: &str = "dogesh";
pub const SHELL_TERMINAL: c_int = STDIN_FILENO;

pub struct Shell {
    pub agent_runtime: Option<Arc<ParkingMutex<dsh_builtin::agent::AgentRuntime>>>,
    pub environment: Arc<RwLock<Environment>>,
    pub exited: Option<ExitStatus>,
    pub pid: Pid,
    pub pgid: Pid,
    pub cmd_history: Option<Arc<ParkingMutex<crate::history::History>>>,
    pub path_history: Option<Arc<ParkingMutex<FrecencyHistory>>>,
    pub(crate) wait_jobs: Vec<Job>,
    /// Wait ownership for async launches: `Active` while the job runs,
    /// retained `Completed(status)` after finalization until `wait`
    /// consumes it. Never snapshotted into helpers — per-`Shell` only.
    pub(crate) known_async: job_ledger::KnownAsyncLedger,
    pub lisp_engine: Rc<RefCell<lisp::LispEngine>>,
    pub(crate) next_job_id: usize,
    pub notebook_session: Option<NotebookSession>,
    pub safety_guard: Arc<crate::safety::SafetyGuard>,
    pub github_status: Option<Arc<RwLock<crate::github::GitHubStatus>>>,
    pub(crate) completion_runtime: Option<Arc<crate::completion::dynamic::CompletionRuntime>>,
    /// Process-substitution helpers this session still owns, in both
    /// directions (`<(...)` producers and `>(...)` consumers). Per-shell so
    /// one shell's shutdown never group-kills another shell's helpers in a
    /// multi-shell process (unit tests). See
    /// `process_substitution::ProcessSubstitutionRegistry`.
    pub process_substitution_registry:
        crate::shell::process_substitution::ProcessSubstitutionRegistry,
    pub session_id: String,
    pending_eval_commands: VecDeque<String>,
    pending_eval_drain_active: Arc<AtomicBool>,
}

pub struct PendingEvalDrainGuard {
    active: Arc<AtomicBool>,
}

impl Drop for PendingEvalDrainGuard {
    fn drop(&mut self) {
        self.active.store(false, Ordering::SeqCst);
    }
}

impl std::fmt::Debug for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shell")
            .field("pid", &self.pid)
            .field("pgid", &self.pgid)
            .finish()
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        // Abnormal-path safety cleanup only — never normal async-exit
        // semantics. Normal async jobs that may outlive this shell must have
        // been explicitly detached via
        // `detach_known_async_jobs_for_normal_exit` before this point;
        // anything still in `wait_jobs` here keeps shell ownership and is
        // killed (unexpected error, panic unwind, early return,
        // infrastructure failure).
        let _ = self.kill_wait_jobs();
        // Producer helpers outlive nothing: group-kill any still-registered
        // group so grandchildren cannot hold session pipes open past exit.
        // Per-shell registry: only this session's lingering helpers, in both
        // directions. Normal-exit `Write` consumers must have been explicitly
        // released via `detach_process_substitution_consumers_for_normal_exit`
        // before this point; anything still registered here is abnormal.
        self.process_substitution_registry
            .cleanup_process_substitution_groups();
    }
}

impl Shell {
    pub fn new(environment: Arc<RwLock<Environment>>) -> Self {
        let pid = getpid();
        // The real process group, not `pid`: dogesh never calls `setpgid(0, 0)`,
        // so when it is started from another shell (or from a test binary) the
        // two differ, and `job_wait` would try to hand the terminal back to a
        // process group that does not exist.
        let pgid = getpgrp();
        let safety_guard = Arc::new(crate::safety::SafetyGuard::new());

        // Initialize Lisp engine
        let lisp_engine = lisp::LispEngine::new(Arc::clone(&environment));

        Shell {
            agent_runtime: None,
            environment,
            exited: None,
            pid,
            pgid,
            cmd_history: None,
            path_history: None,
            wait_jobs: Vec::new(),
            known_async: job_ledger::KnownAsyncLedger::default(),
            lisp_engine,
            next_job_id: 1,
            notebook_session: None,
            safety_guard,
            github_status: None,
            completion_runtime: None,
            process_substitution_registry:
                crate::shell::process_substitution::ProcessSubstitutionRegistry::default(),
            session_id: xid::new().to_string(),
            pending_eval_commands: VecDeque::new(),
            pending_eval_drain_active: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn request_eval_command(&mut self, command: String) -> Result<()> {
        if self.pending_eval_drain_active.load(Ordering::SeqCst) {
            return Err(anyhow::anyhow!(
                "nested command block rerun is not allowed in this version"
            ));
        }
        self.pending_eval_commands.push_back(command);
        Ok(())
    }

    pub fn pop_requested_eval_command(&mut self) -> Option<String> {
        self.pending_eval_commands.pop_front()
    }

    pub fn begin_pending_eval_drain(&self) -> Result<PendingEvalDrainGuard> {
        self.pending_eval_drain_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| {
                anyhow::anyhow!("nested command block rerun is not allowed in this version")
            })?;
        Ok(PendingEvalDrainGuard {
            active: Arc::clone(&self.pending_eval_drain_active),
        })
    }

    pub fn get_next_job_id(&mut self) -> usize {
        job::get_next_job_id(self)
    }

    pub fn set_signals(&mut self) {
        // Handle SIGINT with our custom handler
        use crate::process::signal::install_sigint_handler;
        if let Err(e) = install_sigint_handler() {
            warn!("Failed to install SIGINT handler: {}", e);
        }

        let action = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        unsafe {
            // IGNORE other shell-management logic signals for now
            if let Err(e) = sigaction(Signal::SIGQUIT, &action) {
                warn!("Failed to set SIGQUIT handler: {}", e);
            }
            if let Err(e) = sigaction(Signal::SIGTSTP, &action) {
                warn!("Failed to set SIGTSTP handler: {}", e);
            }
            if let Err(e) = sigaction(Signal::SIGTTIN, &action) {
                warn!("Failed to set SIGTTIN handler: {}", e);
            }
            if let Err(e) = sigaction(Signal::SIGTTOU, &action) {
                warn!("Failed to set SIGTTOU handler: {}", e);
            }
        }
        debug!("Signal handlers setup completed");
    }

    /// Send signal to foreground job
    pub fn send_signal_to_foreground_job(&mut self, signal: Signal) -> Result<()> {
        job::send_signal_to_foreground_job(self, signal)
    }

    /// Terminate all background jobs
    pub fn terminate_background_jobs(&mut self) -> Result<()> {
        job::terminate_background_jobs(self)
    }

    pub fn print_error(&self, msg: String) {
        // unknown command, etc
        eprint!("\r{msg}\r\n");
        std::io::stderr().flush().ok();
    }

    pub async fn eval_str(
        &mut self,
        ctx: &mut Context,
        input: String,
        force_background: bool,
    ) -> Result<i32> {
        // Notebook Hook: Record input if session is active
        if let Some(session) = &mut self.notebook_session
            && session.state == dsh_types::notebook::SessionState::Active
        {
            // Ignore empty input or failures in appending for now (warn only)
            if !input.trim().is_empty() {
                let _ = session.notebook.append_code(&input);
            }
        }

        eval::eval_str(self, ctx, input, force_background).await
    }

    pub fn exit(&mut self) {
        self.exited = Some(ExitStatus::ExitedWith(0));
    }

    pub fn exec_chpwd_hooks(&mut self, pwd: &str) -> Result<()> {
        hooks::exec_chpwd_hooks(self, pwd)
    }

    /// Execute pre-prompt hooks
    pub fn exec_pre_prompt_hooks(&self) -> Result<()> {
        hooks::exec_pre_prompt_hooks(self)
    }

    /// Execute pre-exec hooks
    pub fn exec_pre_exec_hooks(&self, command: &str) -> Result<()> {
        hooks::exec_pre_exec_hooks(self, command)
    }

    /// Execute post-exec hooks
    pub fn exec_post_exec_hooks(&self, command: &str, exit_code: i32) -> Result<()> {
        hooks::exec_post_exec_hooks(self, command, exit_code)
    }

    /// Execute command-not-found hooks
    /// Called when an unknown command is entered
    /// Returns true if a hook handled the command (skipping default error), false otherwise
    pub fn exec_command_not_found_hooks(&self, command: &str) -> bool {
        hooks::exec_command_not_found_hooks(self, command)
    }

    /// Execute completion hooks
    /// Called when a completion is triggered
    pub fn exec_completion_hooks(&self, input: &str, cursor: usize) -> Result<()> {
        hooks::exec_completion_hooks(self, input, cursor)
    }

    /// Execute input-timeout hooks
    /// Called when the user has been idle for a certain period
    pub fn exec_input_timeout_hooks(&self) -> Result<()> {
        hooks::exec_input_timeout_hooks(self)
    }

    pub fn get_job_id(&self) -> usize {
        if self.wait_jobs.is_empty() {
            1
        } else if let Some(job) = self.wait_jobs.last() {
            job.job_id + 1
        } else {
            1
        }
    }

    /// Register a successfully launched async job in one place.
    ///
    /// Records the associated PID in the [`KnownAsyncLedger`](job_ledger)
    /// as `Active`, publishes it as `$!`, and owns the `Job` in
    /// `wait_jobs`. Call only after the spawn succeeded: a spawn-level
    /// failure must leave `$!` and the ledger untouched.
    pub(crate) fn track_async_job(&mut self, job: Job) -> Result<Pid> {
        let pid = job
            .pid
            .ok_or_else(|| anyhow::anyhow!("async job '{}' has no associated PID", job.cmd))?;
        self.known_async.register(pid, job.job_id);
        self.environment.write().last_async_pid = Some(pid.as_raw());
        self.wait_jobs.push(job);
        Ok(pid)
    }

    pub async fn check_job_state(&mut self) -> Result<Vec<Job>> {
        job::check_job_state(self).await
    }

    pub fn kill_wait_jobs(&mut self) -> Result<()> {
        job::kill_wait_jobs(self)
    }

    pub fn open_notebook(&mut self, path: std::path::PathBuf) -> Result<()> {
        self.notebook_session = Some(dsh_types::notebook::NotebookSession::new(path)?);
        Ok(())
    }

    pub fn close_notebook(&mut self) {
        self.notebook_session = None;
    }

    pub fn record_history_outcome(
        &mut self,
        input: &str,
        exit_code: i32,
        duration: std::time::Duration,
        output: Option<&str>,
    ) {
        let Some(history) = &self.cmd_history else {
            return;
        };

        let (processed, ledger_mode, filtered_output) = {
            let env = self.environment.read();
            let ledger_mode = env.variable_state.command_ledger_mode;
            (
                env.policy_state.secret_manager.process_for_history(input),
                ledger_mode,
                (ledger_mode == crate::history::CommandLedgerMode::Output)
                    .then(|| {
                        output.map(|value| env.policy_state.secret_manager.redact_command(value))
                    })
                    .flatten(),
            )
        };
        let Some(command) = processed else {
            return;
        };

        let cwd = std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned());
        let hostname = nix::unistd::gethostname()
            .ok()
            .map(|hostname| hostname.to_string_lossy().into_owned());

        let metadata = HistoryMetadata {
            exit_code: Some(exit_code),
            duration_ms: Some(duration.as_millis() as u64),
            cwd,
            session_id: Some(self.session_id.clone()),
            hostname,
            started_at: chrono::Utc::now().timestamp() - duration.as_secs() as i64,
            author: self
                .environment
                .read()
                .get_var("DOGESH_COMMAND_AUTHOR")
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "human".to_string()),
            output: filtered_output,
            ledger_mode,
        };

        let mut history = history.lock();
        let _ = history.record_outcome(&command, metadata);
    }

    pub fn reload_mcp_config(&self) {
        let mcp_servers = self.environment.read().mcp_servers().to_vec();
        let mcp_manager = self
            .environment
            .read()
            .integration_state
            .mcp_manager
            .clone();

        tokio::spawn(async move {
            let current_servers = mcp_manager.read().server_configs();
            if current_servers == mcp_servers {
                tracing::debug!("MCP config unchanged; skipping reload");
                return;
            }

            tracing::info!(
                "Reloading MCP config (background) with {} servers",
                mcp_servers.len()
            );

            let manager_for_sync = mcp_manager.clone();
            let sync_result = tokio::task::spawn_blocking(move || {
                let mut manager = manager_for_sync.write();
                manager.sync_servers_blocking(mcp_servers)
            })
            .await;

            match sync_result {
                Ok(stats) => {
                    tracing::info!(
                        "MCP config reload complete (added={}, updated={}, removed={}, unchanged={})",
                        stats.added,
                        stats.updated,
                        stats.removed,
                        stats.unchanged
                    );
                }
                Err(err) => {
                    tracing::warn!("MCP config reload worker failed: {}", err);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static SHELL_PROCESS_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn request_eval_command_rejects_nested_drain() {
        let environment = crate::environment::Environment::new();
        let mut shell = Shell::new(environment);

        shell
            .request_eval_command("echo first".to_string())
            .unwrap();
        assert_eq!(
            shell.pop_requested_eval_command().as_deref(),
            Some("echo first")
        );

        let guard = shell.begin_pending_eval_drain().unwrap();
        assert!(
            shell
                .request_eval_command("blocks rerun 1".to_string())
                .is_err()
        );
        assert!(shell.pop_requested_eval_command().is_none());
        drop(guard);

        shell
            .request_eval_command("echo after".to_string())
            .unwrap();
        assert_eq!(
            shell.pop_requested_eval_command().as_deref(),
            Some("echo after")
        );
    }

    /// Shell-level wiring for the foreground output observer: commands run
    /// to completion, the monitors retire, and the final snapshot holds the
    /// data. Chunking and race semantics belong to the `OutputMonitor` unit
    /// tests, so this case issues plain back-to-back writes with no sleeps.
    #[tokio::test]
    async fn foreground_output_observer_captures_stdout_and_stderr() {
        use dsh_types::observed_output::ObservedOutput;

        let _guard = SHELL_PROCESS_TEST_LOCK.lock().await;

        async fn run_observed(command: &str) -> dsh_types::observed_output::ObservedOutputSnapshot {
            let environment = crate::environment::Environment::new();
            let mut shell = Shell::new(environment);
            // Drop both guards before any await below: holding a
            // parking_lot guard across an await can stall the executor.
            {
                let env = shell.environment.read();
                *env.policy_state.safety_level.write() = crate::safety::SafetyLevel::Loose;
            }
            let observer = ObservedOutput::shared(1024);
            let mut ctx = dsh_types::Context::new_safe(shell.pid, shell.pgid, true);
            ctx.interactive = false;
            ctx.output_observer = Some(observer.clone());

            let exit_code = shell
                .eval_str(&mut ctx, command.to_string(), false)
                .await
                .unwrap();
            assert_eq!(exit_code, 0);
            observer.lock().unwrap().snapshot()
        }

        let stdout = run_observed("printf hi").await;
        assert_eq!(stdout.stdout, "hi");
        assert_eq!(stdout.stderr, "");

        let stderr = run_observed("sh -c 'printf err >&2'").await;
        assert_eq!(stderr.stdout, "");
        assert_eq!(stderr.stderr, "err");

        let delayed =
            run_observed("sh -c 'printf out; printf tail; printf err >&2; printf done >&2'").await;
        assert_eq!(delayed.stdout, "outtail");
        assert_eq!(delayed.stderr, "errdone");
    }

    fn exit_zero_builtin(
        _ctx: &Context,
        _argv: Vec<String>,
        _proxy: &mut dyn dsh_builtin::ShellProxy,
    ) -> ExitStatus {
        ExitStatus::ExitedWith(0)
    }

    /// `force_background` runs the whole plan in one isolated helper: every
    /// job of the line shares a single shell environment instead of one
    /// helper per job, and nothing leaks into the parent.
    ///
    /// Needs the sibling `dogesh` binary: run a full `cargo test -p
    /// doge-shell` first, like the other re-exec tests.
    #[tokio::test]
    async fn force_background_runs_whole_plan_in_one_helper() {
        use std::time::Duration;

        let _guard = SHELL_PROCESS_TEST_LOCK.lock().await;

        let environment = crate::environment::Environment::new();
        let mut shell = Shell::new(environment);
        let mut ctx = dsh_types::Context::new_safe(shell.pid, shell.pgid, true);
        // `force_background` is the interactive REPL shortcut: the helper
        // takes the managed-capture path, so its output lands in the job
        // monitors. (Non-interactive helpers inherit the caller fds with
        // no monitor instead.) Synthetic context only — no real terminal
        // is touched: capture triggers on the slot still naming fd 1/2.
        ctx.interactive = true;

        let exit_code = shell
            .eval_str(&mut ctx, "FOO=one; FOO=two; echo $FOO".to_string(), true)
            .await
            .unwrap();
        assert_eq!(exit_code, 0);
        // One managed job for the whole line, not one per `;` job.
        assert_eq!(shell.wait_jobs.len(), 1);
        assert!(!shell.wait_jobs[0].foreground);
        // The parent shell never sees the helper's assignments.
        assert_eq!(
            shell.environment.read().lookup_variable("FOO").as_deref(),
            None
        );

        // The helper's own chain shared one environment: the last job saw
        // `FOO=two`. Poll to completion, then read the capture monitor.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let done = shell.wait_jobs[0].update_status();
            if done {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "whole-plan helper never completed"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        for monitor in shell.wait_jobs[0].monitors.iter_mut() {
            monitor.drain_to_eof().await.expect("drain helper output");
        }
        let captured: String = shell.wait_jobs[0]
            .monitors
            .iter()
            .map(|monitor| monitor.captured_output.clone())
            .collect();
        assert!(
            captured.contains("two"),
            "helper jobs did not share one environment: {captured:?}"
        );
    }

    /// End-to-end background-builtin lifecycle through the real job table:
    /// `waitpid` observation → canonical `BuiltinProcess` state → strict
    /// tree completion → removal from `wait_jobs`.
    ///
    /// The child is a plain `sh` instead of the re-exec helper so the test
    /// never depends on the helper binary; the pid ownership semantics under
    /// test are identical.
    #[tokio::test]
    async fn background_builtin_child_is_removed_after_completion() {
        use crate::process::{BuiltinProcess, JobProcess};
        use std::time::Duration;

        let _guard = SHELL_PROCESS_TEST_LOCK.lock().await;

        let environment = crate::environment::Environment::new();
        let mut shell = Shell::new(environment);

        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn builtin-like child");
        let pid = Pid::from_raw(child.id() as i32);
        // The status belongs to `check_job_state`, not to `Child::wait`.
        std::mem::forget(child);

        let mut job = Job::new("test-builtin &".to_string(), getpgrp());
        job.foreground = false;
        job.pid = Some(pid);
        let mut process = BuiltinProcess::new(
            "test-builtin".to_string(),
            exit_zero_builtin,
            vec!["test-builtin".to_string()],
        );
        process.pid = Some(pid);
        job.set_process(JobProcess::Builtin(process));
        shell.wait_jobs.push(job);
        assert_eq!(shell.wait_jobs.len(), 1);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let completed = shell.check_job_state().await.expect("check job state");
            if completed.len() == 1 && shell.wait_jobs.is_empty() {
                assert_eq!(
                    completed[0].process.as_deref().map(JobProcess::get_state),
                    Some(crate::process::ProcessState::Completed(0, None))
                );
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background builtin was never reaped from the job table \
                 (completed={}, remaining={})",
                completed.len(),
                shell.wait_jobs.len(),
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn background_job_check_does_not_wait_for_stdout_holding_descendant() {
        use std::time::Duration;

        // Process-test serialization: `tokio::sync::Mutex` is an async
        // mutex, so holding its guard across await is the designed usage
        // (waiters queue without stalling the executor). The guard must
        // span the timing-sensitive section below for the bounds to be
        // meaningful under full-suite load.
        let _guard: tokio::sync::MutexGuard<'_, ()> = SHELL_PROCESS_TEST_LOCK.lock().await;

        let environment = crate::environment::Environment::new();
        let mut shell = Shell::new(environment);
        // Drop both guards before any await below: holding a parking_lot
        // guard across an await can stall the executor.
        {
            let env = shell.environment.read();
            *env.policy_state.safety_level.write() = crate::safety::SafetyLevel::Loose;
        }
        let mut ctx = dsh_types::Context::new_safe(shell.pid, shell.pgid, true);
        // Like the `force_background` test above, this exercises the
        // interactive REPL shortcut path (managed capture): the
        // descendant-held pipe below only exists with monitors.
        ctx.interactive = true;

        let exit_code = shell
            .eval_str(&mut ctx, "sh -c '(sleep 1) & exit 0'".to_string(), true)
            .await
            .unwrap();
        assert_eq!(exit_code, 0);
        assert_eq!(shell.wait_jobs.len(), 1);

        // Readiness is polled, never sleep-assumed: a cold helper binary
        // (first exec after relink) may take longer than any fixed sleep
        // to finish spawning, and that startup latency must not flake this
        // test. `update_status` only observes (no table mutation), so the
        // timed reconciliation below still exercises the full drain path.
        let ready = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if shell.wait_jobs[0].update_status() {
                break;
            }
            assert!(
                std::time::Instant::now() < ready,
                "whole-plan helper never completed"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Completed reconciliation uses ReadyNow and therefore never waits
        // for the descendant-held EOF: it retires each monitor with a
        // direct non-blocking drain. The old drain-to-EOF behavior would
        // block ~900ms until `sleep 1` exits.
        let completed = tokio::time::timeout(Duration::from_millis(700), shell.check_job_state())
            .await
            .expect("background job check should not wait for descendant-held stdout")
            .unwrap();

        assert_eq!(completed.len(), 1);
        assert!(shell.wait_jobs.is_empty());
        tokio::time::sleep(Duration::from_millis(1100)).await;
    }
}
