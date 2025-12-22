use crate::environment::Environment;
use anyhow::{Context as _, Result};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use nix::unistd::{Pid, close, dup2, execve, setpgid, tcsetpgrp};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::io::RawFd;
use std::sync::Arc;
use tracing::{debug, error};

use super::job_process::JobProcess;
use super::state::ProcessState;
use super::wait::wait_pid_job;
use crate::shell::SHELL_TERMINAL;
use dsh_types::ExitStatus;

#[derive(Clone, PartialEq, Eq)]
pub struct Process {
    pub(crate) cmd: String,
    pub(crate) argv: Vec<String>,
    pub(crate) pid: Option<Pid>,
    pub(crate) status: Option<ExitStatus>,
    pub(crate) state: ProcessState, // completed, stopped,
    pub next: Option<Box<JobProcess>>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
    pub(crate) cap_stdout: Option<RawFd>,
    pub(crate) cap_stderr: Option<RawFd>,
}

impl std::fmt::Debug for Process {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Process")
            .field("cmd", &self.cmd)
            .field("argv", &self.argv)
            .field("pid", &self.pid)
            .field("status", &self.status)
            .field("state", &self.state)
            .field("next", &self.next)
            .field("stdin", &self.stdin)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}

impl Process {
    pub fn new(cmd: String, argv: Vec<String>) -> Self {
        Process {
            cmd,
            argv,
            pid: None,
            status: None,
            state: ProcessState::Running,
            next: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
            cap_stdout: None,
            cap_stderr: None,
        }
    }

    pub fn set_state(&mut self, pid: Pid, state: ProcessState) -> bool {
        if let Some(ppid) = self.pid
            && ppid == pid
        {
            self.state = state;
            return true;
        }
        if let Some(ref mut next) = self.next
            && next.set_state_pid(pid, state)
        {
            return true;
        }
        false
    }

    pub fn link(&mut self, process: JobProcess) {
        match self.next {
            Some(ref mut p) => {
                p.link(process);
            }
            None => {
                debug!("link:{} next:{}", self.cmd, process.get_cmd());
                self.next = Some(Box::new(process));
            }
        }
    }

    fn set_signals(&self) -> Result<()> {
        debug!("set signal action pid:{:?}", self.pid);
        // Accept job-control-related signals (refer https://www.gnu.org/software/libc/manual/html_node/Launching-Jobs.html)
        let action = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        unsafe {
            sigaction(Signal::SIGINT, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGINT handler: {}", e))?;
            sigaction(Signal::SIGQUIT, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGQUIT handler: {}", e))?;
            sigaction(Signal::SIGTSTP, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGTSTP handler: {}", e))?;
            sigaction(Signal::SIGTTIN, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGTTIN handler: {}", e))?;
            sigaction(Signal::SIGTTOU, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGTTOU handler: {}", e))?;
            sigaction(Signal::SIGCHLD, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGCHLD handler: {}", e))?;
            sigaction(Signal::SIGPIPE, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGPIPE handler: {}", e))?;
        }
        Ok(())
    }

    pub fn launch(
        &mut self,
        pid: Pid,
        pgid: Pid,
        interactive: bool,
        foreground: bool,
        environment: Arc<RwLock<Environment>>,
        pty_slave: Option<RawFd>,
    ) -> Result<()> {
        if interactive {
            // If using PTY, setsid() will be called later which sets the process group/session.
            // We must avoid setpgid() making us a leader before setsid() (which causes EPERM).
            if pty_slave.is_none() {
                debug!(
                    "setpgid child process {} pid:{} pgid:{} foreground:{}",
                    &self.cmd, pid, pgid, foreground
                );
                setpgid(pid, pgid).context("failed setpgid")?;
            } else {
                debug!("Skipping setpgid for PTY process (setsid will handle it)");
            }

            // If we are using PTY, we don't want to set this process as foreground of the SHELL's terminal
            // because the Shell will proxy input/output.
            if foreground && pty_slave.is_none() {
                tcsetpgrp(SHELL_TERMINAL, pgid).context("failed tcsetpgrp")?;
            }

            // Set signals AFTER setting foreground process group to avoid race condition
            // where we receive a signal while still ignoring it (inherited from shell)
            self.set_signals()?;
        } else {
            // For non-interactive/background, we still need to reset SIGPIPE as Rust ignores it by default
            let action = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
            unsafe {
                let _ = sigaction(Signal::SIGPIPE, &action);
            }
        }

        if let Some(slave_fd) = pty_slave {
            // Create a new session and set the controlling terminal to the PTY
            // This is crucial for programs like 'ls' to detect they are in a terminal
            let my_pid = nix::unistd::getpid();
            let my_pgid = nix::unistd::getpgid(Some(my_pid)).unwrap_or(Pid::from_raw(-1));
            let my_sid = nix::unistd::getsid(Some(my_pid)).unwrap_or(Pid::from_raw(-1));
            debug!(
                "setsid check: pid={} pgid={} sid={}",
                my_pid, my_pgid, my_sid
            );

            match nix::unistd::setsid() {
                Ok(new_sid) => {
                    debug!("setsid success: new_sid={}", new_sid);
                }
                Err(e) => {
                    error!("setsid failed raw error: {:?}", e);
                    return Err(anyhow::anyhow!("setsid failed: {}", e));
                }
            }

            unsafe {
                if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) != 0 {
                    // ignore error? sometimes it fails if already leader
                    debug!("ioctl TIOCSCTTY failed (may be already leader)");
                }
            }
        }

        let cmd = CString::new(self.cmd.clone()).context("failed new CString")?;
        let argv: Result<Vec<CString>> = self
            .argv
            .clone()
            .into_iter()
            .map(|a| {
                CString::new(a).map_err(|e| anyhow::anyhow!("failed to create CString: {}", e))
            })
            .collect();
        let argv = argv?;

        // Build environment for child process
        let env_guard = environment.read();
        let mut env_map: HashMap<String, String> = env_guard.system_env_vars.clone();
        for key in &env_guard.exported_vars {
            if let Some(value) = env_guard.variables.get(key) {
                env_map.insert(key.clone(), value.clone());
            }
        }

        // Ensure TERM is set, falling back to xterm if missing or empty
        if let Some(term) = env_map.get("TERM") {
            if term.is_empty() {
                debug!("TERM is empty, defaulting to xterm-256color");
                env_map.insert("TERM".to_string(), "xterm-256color".to_string());
            } else {
                debug!("TERM is set to: {}", term);
            }
        } else {
            debug!("TERM environment variable missing, defaulting to xterm-256color");
            env_map.insert("TERM".to_string(), "xterm-256color".to_string());
        }

        if let Some(ls_colors) = env_map.get("LS_COLORS") {
            debug!("LS_COLORS is set (len: {})", ls_colors.len());
        } else {
            debug!("LS_COLORS is NOT set");
        }

        let envp: Vec<CString> = env_map
            .into_iter()
            .map(|(k, v)| CString::new(format!("{}={}", k, v)).unwrap())
            .collect();

        debug!(
            "launch: execve cmd:{:?} argv:{:?} foreground:{:?} infile:{:?} outfile:{:?} pid:{:?} pgid:{:?} pty:{:?}",
            cmd, argv, foreground, self.stdin, self.stdout, pid, pgid, pty_slave
        );

        // Standard IO setup (PTY slave is handled via self.stdin/stdout/stderr being set to it by caller if needed)

        // 1. Handle STDIN
        if self.stdin != STDIN_FILENO {
            dup2(self.stdin, STDIN_FILENO)
                .map_err(|e| anyhow::anyhow!("dup2 stdin failed: {}", e))?;
        }
        // Don't close stdin yet if it matches stdout or stderr, as we need it for subsequent dup2 calls
        let keep_stdin = self.stdin == self.stdout || self.stdin == self.stderr;
        if self.stdin > 2 && !keep_stdin {
            close(self.stdin).map_err(|e| anyhow::anyhow!("close stdin failed: {}", e))?;
        }

        // 2. Handle STDOUT & STDERR
        if self.stdout == self.stderr {
            // Combined stdout/stderr (e.g. PTY or redirected to same file)
            if self.stdout != STDOUT_FILENO {
                dup2(self.stdout, STDOUT_FILENO)
                    .map_err(|e| anyhow::anyhow!("dup2 stdout failed: {}", e))?;
            }
            if self.stderr != STDERR_FILENO {
                dup2(self.stderr, STDERR_FILENO)
                    .map_err(|e| anyhow::anyhow!("dup2 stderr failed: {}", e))?;
            }

            // Close the source if it is > 2.
            // Even if it was kept open from stdin check above, we are now done with it.
            if self.stdout > 2 {
                close(self.stdout).map_err(|e| anyhow::anyhow!("close stdout failed: {}", e))?;
            }
        } else {
            // Separate stdout/stderr
            if self.stdout != STDOUT_FILENO {
                dup2(self.stdout, STDOUT_FILENO)
                    .map_err(|e| anyhow::anyhow!("dup2 stdout failed: {}", e))?;
                // If stdout matched stdin, it was kept open. Now we can close it.
                if self.stdout > 2 {
                    close(self.stdout)
                        .map_err(|e| anyhow::anyhow!("close stdout failed: {}", e))?;
                }
            }

            if self.stderr != STDERR_FILENO {
                dup2(self.stderr, STDERR_FILENO)
                    .map_err(|e| anyhow::anyhow!("dup2 stderr failed: {}", e))?;
                // If stderr matched stdin, it was kept open. Now close it.
                if self.stderr > 2 {
                    close(self.stderr)
                        .map_err(|e| anyhow::anyhow!("close stderr failed: {}", e))?;
                }
            }
        }
        match execve(&cmd, &argv, &envp) {
            Ok(_) => Ok(()),
            Err(nix::errno::Errno::EACCES) => {
                error!("Failed to exec {:?} (EACCESS). chmod(1) may help.", cmd);
                std::process::exit(1);
            }
            Err(err) => {
                error!("Failed to exec {:?} ({})", cmd, err);
                std::process::exit(1);
            }
        }
    }

    pub(crate) fn update_state(&mut self) -> Option<ProcessState> {
        if let ProcessState::Completed(_, _) = self.state {
            Some(self.state)
        } else {
            if let Some(pid) = self.pid
                && let Some((_waited_pid, state)) = wait_pid_job(pid, true)
            {
                self.state = state;
            }

            if let Some(next) = self.next.as_mut() {
                next.update_state();
            }

            Some(self.state)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::signal::Signal;
    use nix::unistd::Pid;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn test_process_state_transitions() {
        init();
        let mut process = Process::new("test_cmd".to_string(), vec!["arg1".to_string()]);

        // Initial state is Running
        assert!(matches!(process.state, ProcessState::Running));

        // State change test
        process.state = ProcessState::Completed(0, None);
        assert!(matches!(process.state, ProcessState::Completed(0, None)));

        process.state = ProcessState::Stopped(Pid::from_raw(1234), Signal::SIGSTOP);
        assert!(matches!(
            process.state,
            ProcessState::Stopped(_, Signal::SIGSTOP)
        ));
    }
}
