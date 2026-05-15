pub mod eval;
pub mod hooks;
pub mod job;
pub mod parse;
pub mod terminal;

use crate::environment::Environment;
use crate::history::{FrecencyHistory, HistoryMetadata};
use crate::lisp;
use crate::process::Job;
use anyhow::Result;
use dsh_types::notebook::NotebookSession;
use dsh_types::{Context, ExitStatus};
use libc::{STDIN_FILENO, c_int};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use nix::unistd::{Pid, getpid};
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

pub const APP_NAME: &str = "dsh";
pub const SHELL_TERMINAL: c_int = STDIN_FILENO;

pub struct Shell {
    pub environment: Arc<RwLock<Environment>>,
    pub exited: Option<ExitStatus>,
    pub pid: Pid,
    pub pgid: Pid,
    pub cmd_history: Option<Arc<ParkingMutex<crate::history::History>>>,
    pub path_history: Option<Arc<ParkingMutex<FrecencyHistory>>>,
    pub(crate) wait_jobs: Vec<Job>,
    pub lisp_engine: Rc<RefCell<lisp::LispEngine>>,
    pub(crate) next_job_id: usize,
    pub notebook_session: Option<NotebookSession>,
    pub safety_guard: Arc<crate::safety::SafetyGuard>,
    pub github_status: Option<Arc<RwLock<crate::github::GitHubStatus>>>,
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
        let _ = self.kill_wait_jobs();
    }
}

impl Shell {
    pub fn new(environment: Arc<RwLock<Environment>>) -> Self {
        let pid = getpid();
        let pgid = pid;
        let safety_guard = Arc::new(crate::safety::SafetyGuard::new());

        // Initialize Lisp engine
        let lisp_engine = lisp::LispEngine::new(Arc::clone(&environment));

        Shell {
            environment,
            exited: None,
            pid,
            pgid,
            cmd_history: None,
            path_history: None,
            wait_jobs: Vec::new(),
            lisp_engine,
            next_job_id: 1,
            notebook_session: None,
            safety_guard,
            github_status: None,
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
    #[allow(dead_code)]
    pub fn send_signal_to_foreground_job(&mut self, signal: Signal) -> Result<()> {
        job::send_signal_to_foreground_job(self, signal)
    }

    /// Terminate all background jobs
    #[allow(dead_code)]
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

    fn launch_subshell(&mut self, ctx: &mut Context, jobs: Vec<Job>) -> Result<()> {
        eval::launch_subshell(self, ctx, jobs)
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
    ) {
        let Some(history) = &self.cmd_history else {
            return;
        };

        let processed = {
            let env = self.environment.read();
            env.secret_manager.process_for_history(input)
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
        };

        let mut history = history.lock();
        let _ = history.record_outcome(&command, metadata);
    }

    pub fn reload_mcp_config(&self) {
        let mcp_servers = self.environment.read().mcp_servers().to_vec();
        let mcp_manager = self.environment.read().mcp_manager.clone();

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

    #[tokio::test]
    async fn foreground_output_observer_captures_stdout_and_stderr() {
        use dsh_types::observed_output::ObservedOutput;

        async fn run_observed(command: &str) -> dsh_types::observed_output::ObservedOutputSnapshot {
            let environment = crate::environment::Environment::new();
            let mut shell = Shell::new(environment);
            *shell.environment.read().safety_level.write() = crate::safety::SafetyLevel::Loose;
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
    }
}
