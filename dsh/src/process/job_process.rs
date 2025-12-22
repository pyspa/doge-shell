use anyhow::{Context as _, Result};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::signal::Signal;
use nix::unistd::{Pid, getpid, pipe};
use std::os::unix::io::RawFd;
use tracing::debug;

use super::builtin::BuiltinProcess;
use super::fork::{fork_builtin_process, fork_process};
use super::io::{create_pipe, handle_output_redirect};
use super::process::Process;
use super::redirect::Redirect;
use super::signal::send_signal;
use super::state::ProcessState;
use crate::shell::Shell;
use dsh_builtin::ShellProxy;
use dsh_types::Context;

#[derive(Clone, PartialEq, Eq)]
pub enum JobProcess {
    Builtin(BuiltinProcess),
    Command(Process),
}

impl std::fmt::Debug for JobProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        match self {
            JobProcess::Builtin(jprocess) => f
                .debug_struct("JobProcess::Builtin")
                .field("cmd", &jprocess.argv)
                .field("has_next", &jprocess.next.is_some())
                .finish(),
            JobProcess::Command(jprocess) => f
                .debug_struct("JobProcess::Command")
                .field("cmd", &jprocess.cmd)
                .field("argv", &jprocess.argv)
                .field("pid", &jprocess.pid)
                .field("stdin", &jprocess.stdin)
                .field("stdout", &jprocess.stdout)
                .field("stderr", &jprocess.stderr)
                .field("has_next", &jprocess.next.is_some())
                .field("state", &jprocess.state)
                .finish(),
        }
    }
}

impl JobProcess {
    pub fn link(&mut self, process: JobProcess) {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.link(process),
            JobProcess::Command(jprocess) => jprocess.link(process),
        }
    }

    pub fn next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    #[allow(dead_code)]
    pub fn mut_next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    pub fn take_next(&mut self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.take(),
            JobProcess::Command(jprocess) => jprocess.next.take(),
        }
    }

    pub fn set_io(&mut self, stdin: RawFd, stdout: RawFd, stderr: RawFd) {
        match self {
            JobProcess::Builtin(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
                jprocess.stderr = stderr;
            }
            JobProcess::Command(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
                jprocess.stderr = stderr;
            }
        }
    }

    pub fn get_io(&self) -> (RawFd, RawFd, RawFd) {
        match self {
            JobProcess::Builtin(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
            JobProcess::Command(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
        }
    }

    pub fn set_pid(&mut self, pid: Option<Pid>) {
        match self {
            JobProcess::Builtin(_) => {
                // noop
            }
            JobProcess::Command(process) => {
                process.pid = pid;
            }
        }
    }

    #[allow(dead_code)]
    pub fn get_pid(&self) -> Option<Pid> {
        match self {
            JobProcess::Builtin(_) => {
                // noop
                None
            }
            JobProcess::Command(process) => process.pid,
        }
    }

    #[allow(dead_code)]
    pub fn set_state(&mut self, state: ProcessState) {
        match self {
            JobProcess::Builtin(p) => p.state = state,
            JobProcess::Command(p) => p.state = state,
        }
    }

    pub(crate) fn set_state_pid(&mut self, pid: Pid, state: ProcessState) -> bool {
        debug!(
            "🔄 STATE: set_state_pid called for pid: {}, state: {:?}",
            pid, state
        );
        let result = match self {
            JobProcess::Builtin(p) => {
                debug!("🔄 STATE: Setting state for builtin process: {}", p.name);
                p.set_state(pid, state)
            }
            JobProcess::Command(p) => {
                debug!("🔄 STATE: Setting state for command process: {}", p.cmd);
                p.set_state(pid, state)
            }
        };
        debug!("🔄 STATE: set_state_pid result: {}", result);
        result
    }

    pub fn get_state(&self) -> ProcessState {
        match self {
            JobProcess::Builtin(p) => p.state,
            JobProcess::Command(p) => p.state,
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        if self.get_state() == ProcessState::Running {
            return false;
        }
        if let Some(p) = self.next() {
            return p.is_stopped();
        }
        true
    }

    pub(crate) fn is_completed(&self) -> bool {
        match self.get_state() {
            ProcessState::Completed(_, _signal) => {
                //ok
                if let Some(next) = self.next() {
                    return next.is_completed();
                }
            }
            _ => {
                return false;
            }
        };
        true
    }

    /// Check if pipeline should be considered complete based on consumer process termination
    /// This handles the case where the last process in a pipeline (consumer) exits normally
    /// while earlier processes (producers) are still running
    pub(crate) fn is_pipeline_consumer_terminated(&self) -> bool {
        // If this process has a next process, check if the consumer terminated
        if let Some(next) = self.next() {
            // Recursively check the next process
            if next.is_pipeline_consumer_terminated() {
                return true;
            }
            // If the next process (consumer) completed normally, the pipeline should terminate
            if let ProcessState::Completed(0, None) = next.get_state() {
                debug!(
                    "PIPELINE_CONSUMER_TERMINATED: Consumer process '{}' exited normally, pipeline should terminate",
                    next.get_cmd()
                );
                return true;
            }
        }
        false
    }

    /// Check if any process in the pipeline is stopped
    pub(crate) fn has_stopped_process(&self) -> bool {
        match self.get_state() {
            ProcessState::Stopped(_, _) => true,
            _ => {
                if let Some(next) = self.next() {
                    next.has_stopped_process()
                } else {
                    false
                }
            }
        }
    }

    pub fn get_cap_out(&self) -> (Option<RawFd>, Option<RawFd>) {
        match self {
            JobProcess::Builtin(p) => (p.cap_stdout, p.cap_stderr),
            JobProcess::Command(p) => (p.cap_stdout, p.cap_stderr),
        }
    }

    pub fn get_cmd(&self) -> &str {
        match self {
            JobProcess::Builtin(p) => &p.name,
            JobProcess::Command(p) => &p.cmd,
        }
    }

    pub fn check_safety(&self, shell: &mut Shell) -> Result<bool> {
        let (cmd, argv) = match self {
            JobProcess::Builtin(p) => (p.name.as_str(), &p.argv),
            JobProcess::Command(p) => (p.cmd.as_str(), &p.argv),
        };

        let level = shell.environment.read().safety_level.read().clone();

        match shell.safety_guard.check_command(&level, cmd, argv) {
            crate::safety::SafetyResult::Allowed => {}
            crate::safety::SafetyResult::Denied(reason) => {
                shell.print_error(format!("Safety Guard: Execution denied: {}", reason));
                return Ok(false);
            }
            crate::safety::SafetyResult::Confirm(message) => {
                if !shell.confirm_action(&message)? {
                    return Ok(false);
                }
            }
        }

        if let Some(next) = self.next() {
            return next.check_safety(shell);
        }

        Ok(true)
    }

    #[allow(dead_code)]
    pub fn waitable(&self) -> bool {
        matches!(self, JobProcess::Command(_))
    }

    pub fn launch(
        &mut self,
        ctx: &mut Context,
        shell: &mut Shell,
        redirect: &Option<Redirect>,
        stdout: RawFd,
        pty_slave: Option<RawFd>,
    ) -> Result<(Pid, Option<Box<JobProcess>>)> {
        // has pipelines process ?
        let next_process = self.take_next();

        let pipe_out = match next_process {
            Some(_) => {
                create_pipe(ctx)? // create pipe
            }
            None => {
                // Automatic capture for non-interactive mode (e.g. smart pipe tests)
                // We don't do this in interactive mode to preserve TTY (colors, etc.)
                if !ctx.interactive && redirect.is_none() && pty_slave.is_none() {
                    let (pout, pin) = pipe().context("failed pipe")?;
                    ctx.outfile = pin;
                    match self {
                        JobProcess::Builtin(p) => p.cap_stdout = Some(pout),
                        JobProcess::Command(p) => p.cap_stdout = Some(pout),
                    }
                    None
                } else {
                    // Manual capture or redirect
                    handle_output_redirect(ctx, redirect, stdout)?
                }
            }
        };

        if let Some(slave) = pty_slave {
            // PTY sets the "default" TTY fds.
            // Pipeline/Redirection/Capture overrides them.
            // We should only set slave if the FD is still the default.
            // In non-interactive mode with redirects, we MUST favor redirects
            // to ensure captured output works correctly.

            let mut slave_applied = false;
            if ctx.infile == STDIN_FILENO {
                ctx.infile = slave;
                slave_applied = true;
            }
            if ctx.outfile == STDOUT_FILENO {
                ctx.outfile = slave;
                slave_applied = true;
            }
            if ctx.errfile == STDERR_FILENO {
                ctx.errfile = slave;
                slave_applied = true;
            }

            debug!(
                "PTY_IO_SETUP: Job {} ({}) - final i/o: infile={}, outfile={}, errfile={} (slave={}, slave_applied={})",
                shell.get_job_id(),
                self.get_cmd(),
                ctx.infile,
                ctx.outfile,
                ctx.errfile,
                slave,
                slave_applied
            );
        }

        self.set_io(ctx.infile, ctx.outfile, ctx.errfile);

        // initial pid
        let current_pid = getpid();

        let pid = match self {
            JobProcess::Builtin(process) => {
                if ctx.foreground {
                    process.pid = Some(current_pid);
                    process.launch(ctx, shell)?;
                    current_pid
                } else {
                    // Fork for background execution
                    let child_pid = fork_builtin_process(ctx, process, shell)?;
                    process.pid = Some(child_pid);
                    child_pid
                }
            }
            JobProcess::Command(process) => {
                ctx.process_count += 1;
                // fork
                fork_process(ctx, ctx.pgid, process, shell, pty_slave)?
            }
        };

        self.set_pid(Some(pid));

        // set pipe inout
        if let Some(pipe_out) = pipe_out {
            ctx.infile = pipe_out;
        }
        // return launched process pid and pipeline process
        Ok((pid, next_process))
    }

    pub fn kill(&self) -> Result<()> {
        match self {
            JobProcess::Builtin(_) => Ok(()),
            JobProcess::Command(process) => {
                if let Some(pid) = process.pid {
                    send_signal(pid, Signal::SIGKILL)
                } else {
                    Ok(())
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn cont(&self) -> Result<()> {
        match self {
            JobProcess::Builtin(_) => Ok(()),
            JobProcess::Command(process) => {
                if let Some(pid) = process.pid {
                    debug!("send signal SIGCONT pid:{:?}", pid);
                    send_signal(pid, Signal::SIGCONT)
                } else {
                    Ok(())
                }
            }
        }
    }

    pub(crate) fn update_state(&mut self) -> Option<ProcessState> {
        match self {
            JobProcess::Builtin(process) => process.update_state(),
            JobProcess::Command(process) => process.update_state(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::state::ProcessState;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn test_pipeline_consumer_termination() {
        init();

        // Create a pipeline: cat | less
        let mut cat_process = Process::new("cat".to_string(), vec!["cat".to_string()]);
        let mut less_process = Process::new("less".to_string(), vec!["less".to_string()]);

        // Set initial states
        cat_process.state = ProcessState::Running;
        less_process.state = ProcessState::Running;

        // Link them in pipeline
        cat_process.next = Some(Box::new(JobProcess::Command(less_process.clone())));

        let mut cat_job_process = JobProcess::Command(cat_process);

        // Initially, consumer is not terminated
        assert!(!cat_job_process.is_pipeline_consumer_terminated());

        // Now simulate less (consumer) exiting normally
        if let JobProcess::Command(cat_proc) = &mut cat_job_process
            && let Some(next_box) = &mut cat_proc.next
            && let JobProcess::Command(less_proc) = next_box.as_mut()
        {
            less_proc.state = ProcessState::Completed(0, None);
        }

        // Now consumer should be detected as terminated
        assert!(cat_job_process.is_pipeline_consumer_terminated());

        // But the pipeline is not fully completed since cat is still running
        assert!(!cat_job_process.is_completed());
    }

    #[test]
    fn test_job_process_variants() {
        init();
        let process = Process::new("test".to_string(), vec![]);
        let job_process = JobProcess::Command(process);

        // JobProcess type check
        match job_process {
            JobProcess::Command(_) => {} // Expected variant
            _ => panic!("Expected Command variant"),
        }
    }
}
