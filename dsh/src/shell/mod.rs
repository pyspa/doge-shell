pub mod eval;
pub mod hooks;
pub mod job;
pub mod parse;
pub mod terminal;

use crate::environment::Environment;
use crate::history::FrecencyHistory;
use crate::lisp;
use crate::notebook::NotebookSession;
use crate::process::Job;
use anyhow::Result;
use dsh_builtin::McpManager;
use dsh_types::{Context, ExitStatus};
use libc::{STDIN_FILENO, c_int};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use nix::unistd::{Pid, getpid};
use parking_lot::Mutex as ParkingMutex;
use parking_lot::RwLock;
use std::io::Write;
use std::sync::Arc;
use std::{cell::RefCell, rc::Rc};
use tracing::{debug, warn};

pub const APP_NAME: &str = "dsh";
pub const SHELL_TERMINAL: c_int = STDIN_FILENO;

pub struct Shell {
    pub environment: Arc<RwLock<Environment>>,
    pub exited: Option<ExitStatus>,
    pub pid: Pid,
    pub pgid: Pid,
    pub cmd_history: Option<Arc<ParkingMutex<FrecencyHistory>>>,
    pub path_history: Option<Arc<ParkingMutex<FrecencyHistory>>>,
    pub(crate) wait_jobs: Vec<Job>,
    pub lisp_engine: Rc<RefCell<lisp::LispEngine>>,
    pub(crate) next_job_id: usize,
    pub notebook_session: Option<NotebookSession>,
    pub safety_guard: crate::safety::SafetyGuard,
    pub mcp_manager: Arc<RwLock<McpManager>>,
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
        let safety_guard = crate::safety::SafetyGuard::new();

        // Initialize McpManager
        // Start with an default empty manager wrapped in RwLock.
        // The actual configuration will be loaded later via `setup_mcp_config`.
        let mcp_manager = Arc::new(RwLock::new(McpManager::default()));

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
            mcp_manager,
        }
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
            && session.state == crate::notebook::SessionState::Active
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
        self.notebook_session = Some(crate::notebook::NotebookSession::new(path)?);
        Ok(())
    }

    pub fn close_notebook(&mut self) {
        self.notebook_session = None;
    }

    pub fn reload_mcp_config(&self) {
        let mcp_servers = self.environment.read().mcp_servers().to_vec();
        tracing::debug!("Reloading MCP config with {} servers", mcp_servers.len());
        // McpManager::load calls build_from_servers (now modified to not use TOML)
        let new_manager = McpManager::load(mcp_servers);
        *self.mcp_manager.write() = new_manager;
    }
}
