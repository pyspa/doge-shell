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
    pub(crate) cap_stdout: Option<RawFd>,
    pub(crate) cap_stderr: Option<RawFd>,
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
            cap_stdout: None,
            cap_stderr: None,
        }
    }

    pub fn set_state(&mut self, pid: Pid, state: ProcessState) -> bool {
        if let Some(self_pid) = self.pid
            && self_pid == pid
        {
            self.state = state;
            return true;
        }

        if let Some(ref mut next) = self.next {
            return next.set_state_pid(pid, state);
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
                    self.state = ProcessState::Completed(code.clamp(0, 255) as u8, None);
                } else {
                    self.state = ProcessState::Completed(1, None);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;

    fn test_context() -> Context {
        Context::new_safe(Pid::from_raw(1), Pid::from_raw(1), true)
    }

    fn test_shell() -> Shell {
        Shell::new(Environment::new())
    }

    fn builtin_exit_zero(
        _ctx: &Context,
        _argv: Vec<String>,
        _proxy: &mut dyn dsh_builtin::ShellProxy,
    ) -> ExitStatus {
        ExitStatus::ExitedWith(0)
    }

    fn builtin_exit_seven(
        _ctx: &Context,
        _argv: Vec<String>,
        _proxy: &mut dyn dsh_builtin::ShellProxy,
    ) -> ExitStatus {
        ExitStatus::ExitedWith(7)
    }

    fn builtin_exit_negative(
        _ctx: &Context,
        _argv: Vec<String>,
        _proxy: &mut dyn dsh_builtin::ShellProxy,
    ) -> ExitStatus {
        ExitStatus::ExitedWith(-1)
    }

    fn launch_state(
        cmd_fn: fn(&Context, Vec<String>, &mut dyn dsh_builtin::ShellProxy) -> ExitStatus,
    ) -> ProcessState {
        let mut process = BuiltinProcess::new(
            "test-builtin".to_string(),
            cmd_fn,
            vec!["test-builtin".into()],
        );
        let mut ctx = test_context();
        let mut shell = test_shell();

        process
            .launch(&mut ctx, &mut shell)
            .expect("builtin launch should succeed");

        process.state
    }

    #[test]
    fn launch_preserves_success_exit_code() {
        assert_eq!(
            launch_state(builtin_exit_zero),
            ProcessState::Completed(0, None)
        );
    }

    #[test]
    fn launch_preserves_nonzero_exit_code() {
        assert_eq!(
            launch_state(builtin_exit_seven),
            ProcessState::Completed(7, None)
        );
    }

    #[test]
    fn launch_maps_negative_exit_code_to_failure() {
        assert_eq!(
            launch_state(builtin_exit_negative),
            ProcessState::Completed(1, None)
        );
    }
}
