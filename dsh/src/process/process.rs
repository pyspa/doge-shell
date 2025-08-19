use anyhow::{Context as _, Result};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use nix::unistd::{Pid, close, dup2, execv, setpgid, tcsetpgrp};
use std::ffi::CString;
use std::os::unix::io::RawFd;
use tracing::{debug, error};

use super::io::copy_fd;
use super::job_process::JobProcess;
use super::state::ProcessState;
use super::wait::wait_pid_job;
use crate::shell::SHELL_TERMINAL;
use dsh_types::ExitStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
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
        }
        Ok(())
    }

    pub fn launch(
        &mut self,
        pid: Pid,
        pgid: Pid,
        interactive: bool,
        foreground: bool,
    ) -> Result<()> {
        if interactive {
            debug!(
                "setpgid child process {} pid:{} pgid:{} foreground:{}",
                &self.cmd, pid, pgid, foreground
            );
            setpgid(pid, pgid).context("failed setpgid")?;

            if foreground {
                tcsetpgrp(SHELL_TERMINAL, pgid).context("failed tcsetpgrp")?;
            }

            self.set_signals()?;
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

        debug!(
            "launch: execv cmd:{:?} argv:{:?} foreground:{:?} infile:{:?} outfile:{:?} pid:{:?} pgid:{:?}",
            cmd, argv, foreground, self.stdin, self.stdout, pid, pgid,
        );

        copy_fd(self.stdin, STDIN_FILENO)?;
        if self.stdout == self.stderr {
            dup2(self.stdout, STDOUT_FILENO)
                .map_err(|e| anyhow::anyhow!("dup2 stdout failed: {}", e))?;
            dup2(self.stderr, STDERR_FILENO)
                .map_err(|e| anyhow::anyhow!("dup2 stderr failed: {}", e))?;
            close(self.stdout).map_err(|e| anyhow::anyhow!("close stdout failed: {}", e))?;
        } else {
            copy_fd(self.stdout, STDOUT_FILENO)?;
            copy_fd(self.stderr, STDERR_FILENO)?;
        }
        match execv(&cmd, &argv) {
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
