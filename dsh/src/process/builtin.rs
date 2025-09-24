use anyhow::Result;
use dsh_types::{Context, ExitStatus};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::unistd::Pid;
use std::os::unix::io::RawFd;
use tracing::debug;

use super::job_process::JobProcess;
use super::state::ProcessState;
use crate::shell::Shell;

#[derive(Clone)]
pub struct BuiltinProcess {
    pub(crate) name: String,
    pub(crate) cmd_fn: fn(&Context, Vec<String>, &mut dyn dsh_builtin::ShellProxy) -> ExitStatus,
    pub(crate) argv: Vec<String>,
    pub(crate) state: ProcessState, // completed, stopped,
    pub pid: Option<Pid>,
    pub next: Option<Box<JobProcess>>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
}

impl PartialEq for BuiltinProcess {
    fn eq(&self, other: &Self) -> bool {
        self.argv == other.argv
    }
}

impl Eq for BuiltinProcess {}

impl std::fmt::Debug for BuiltinProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("BuiltinProcess")
            .field("argv", &self.argv)
            .field("state", &self.state)
            .field("pid", &self.pid)
            .field("next", &self.next)
            .field("stdin", &self.stdin)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}

impl BuiltinProcess {
    pub fn new(
        name: String,
        cmd_fn: fn(&Context, Vec<String>, &mut dyn dsh_builtin::ShellProxy) -> ExitStatus,
        argv: Vec<String>,
    ) -> Self {
        BuiltinProcess {
            name,
            cmd_fn,
            argv,
            state: ProcessState::Running,
            pid: None,
            next: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
        }
    }

    pub fn set_state(&mut self, pid: Pid, state: ProcessState) -> bool {
        if let Some(ref mut next) = self.next {
            if next.set_state_pid(pid, state) {
                return true;
            }
            // Check if this process matches the PID
            if let Some(self_pid) = self.pid
                && self_pid == pid
            {
                self.state = state;
                return true;
            }
        }
        false
    }

    pub fn link(&mut self, process: JobProcess) {
        match self.next {
            Some(ref mut p) => {
                p.link(process);
            }
            None => {
                self.next = Some(Box::new(process));
            }
        }
    }

    pub fn launch(&mut self, ctx: &mut Context, shell: &mut Shell) -> Result<()> {
        let exit = (self.cmd_fn)(ctx, self.argv.to_vec(), shell);
        match exit {
            ExitStatus::ExitedWith(code) => {
                if code >= 0 {
                    self.state = ProcessState::Completed(1, None);
                } else {
                    self.state = ProcessState::Completed(0, None);
                }
                debug!("Builtin process {} exited with code: {}", self.name, code);
            }
            ExitStatus::Running(_pid) => {
                self.state = ProcessState::Running;
                debug!("Builtin process {} is running", self.name);
            }
            ExitStatus::Break | ExitStatus::Continue | ExitStatus::Return => {
                self.state = ProcessState::Completed(0, None);
                debug!(
                    "Builtin process {} completed with control flow: {:?}",
                    self.name, exit
                );
            }
        }
        Ok(())
    }

    pub(crate) fn update_state(&mut self) -> Option<ProcessState> {
        if let Some(next) = self.next.as_mut() {
            next.update_state()
        } else {
            None
        }
    }
}
