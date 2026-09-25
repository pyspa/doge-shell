use anyhow::Result;
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::termios::Termios;
use nix::unistd::Pid;
use std::fmt::Debug;
use std::fs::File;
use std::io::Write;
use std::mem;
use std::os::fd::BorrowedFd;
use std::os::unix::io::FromRawFd;
use std::os::unix::io::RawFd;
use tracing::warn;

pub mod terminal;
pub use terminal::{ShellMode, TerminalState};
pub mod agent;
pub mod ansi;
pub mod command_block;
pub mod completion;
pub mod cron;
pub mod mcp;
pub mod notebook;
pub mod observed_output;
pub mod output_history;
pub mod output_schema;
pub mod output_text;
pub mod placeholder;
pub mod process_runtime;
pub mod project;
pub mod quick_fix;
pub mod safety_policy;
pub mod schedule;
pub mod shell_options;
pub mod snippet;
pub mod text;
pub use project::Project;
pub use shell_options::{ShellOption, ShellOptions};

#[derive(Clone)]
pub struct Context {
    pub shell_pid: Pid,
    pub shell_pgid: Pid,
    pub shell_tmode: Option<Termios>,
    pub terminal_state: TerminalState,
    pub shell_mode: ShellMode,
    pub foreground: bool,
    pub interactive: bool,
    pub infile: RawFd,
    pub outfile: RawFd,
    pub errfile: RawFd,
    pub captured_out: Option<RawFd>,
    pub output_observer: Option<observed_output::SharedOutputObserver>,
    pub save_history: bool,
    pub pid: Option<Pid>,
    pub pgid: Option<Pid>,
    pub process_count: u32,
}

impl Context {
    pub fn new(
        shell_pid: Pid,
        shell_pgid: Pid,
        shell_tmode: Option<Termios>,
        foreground: bool,
    ) -> Self {
        let terminal_state = TerminalState::detect(STDIN_FILENO);
        let shell_mode = ShellMode::detect();

        Context {
            shell_pid,
            shell_pgid,
            shell_tmode,
            terminal_state: terminal_state.clone(),
            shell_mode,
            foreground,
            interactive: terminal_state.is_terminal,
            infile: STDIN_FILENO,
            outfile: STDOUT_FILENO,
            errfile: STDERR_FILENO,
            captured_out: None,
            output_observer: None,
            save_history: true,
            pid: None,
            pgid: None,
            process_count: 0,
        }
    }

    /// Safe Context creation (with terminal detection)
    pub fn new_safe(shell_pid: Pid, shell_pgid: Pid, foreground: bool) -> Self {
        let terminal_state = TerminalState::detect(STDIN_FILENO);
        let shell_mode = ShellMode::detect();

        // Try to get terminal settings, but handle the case where no TTY is available
        let shell_tmode = if let Some(tmodes) = terminal_state.get_tmodes() {
            Some(tmodes.clone())
        } else {
            use nix::sys::termios::tcgetattr;

            // Try standard file descriptors in sequence
            // Try standard file descriptors in sequence
            tcgetattr(unsafe { BorrowedFd::borrow_raw(STDIN_FILENO) })
                .or_else(|_| tcgetattr(unsafe { BorrowedFd::borrow_raw(STDOUT_FILENO) }))
                .or_else(|_| tcgetattr(unsafe { BorrowedFd::borrow_raw(STDERR_FILENO) }))
                .or_else(|_| {
                    // If standard file descriptors don't have terminal settings,
                    // try /dev/tty as a last resort
                    use nix::fcntl::{OFlag, open};
                    use nix::sys::stat::Mode;

                    match open("/dev/tty", OFlag::O_RDONLY, Mode::empty()) {
                        Ok(tty_fd) => tcgetattr(tty_fd),
                        Err(_) => Err(nix::errno::Errno::ENOTTY),
                    }
                })
                .ok()
        };
        if shell_tmode.is_none() {
            warn!("No TTY available; running without terminal settings");
        }

        Context {
            shell_pid,
            shell_pgid,
            shell_tmode,
            terminal_state: terminal_state.clone(),
            shell_mode,
            foreground,
            interactive: terminal_state.is_terminal,
            infile: STDIN_FILENO,
            outfile: STDOUT_FILENO,
            errfile: STDERR_FILENO,
            captured_out: None,
            output_observer: None,
            save_history: true,
            pid: None,
            pgid: None,
            process_count: 0,
        }
    }

    /// Check if job control is enabled for this execution context.
    ///
    /// Terminal capability (`terminal_state`) and shell mode alone are not
    /// enough: a non-interactive `-c` invocation on a TTY must still count
    /// as disabled, so background helpers default their stdin to `/dev/null`
    /// there too. `interactive` is the commit point for that decision.
    pub fn supports_job_control(&self) -> bool {
        self.interactive
            && self.terminal_state.supports_job_control
            && self.shell_mode.supports_job_control()
    }

    /// Check if in interactive mode
    pub fn is_interactive_mode(&self) -> bool {
        self.shell_mode.is_interactive()
    }
}

impl Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("Context")
            .field("shell_pid", &self.shell_pid)
            .field("shell_pgid", &self.shell_pgid)
            .field("terminal_state", &self.terminal_state)
            .field("shell_mode", &self.shell_mode)
            .field("foreground", &self.foreground)
            .field("interactive", &self.interactive)
            .field("infile", &self.infile)
            .field("outfile", &self.outfile)
            .field("errfile", &self.errfile)
            .field("captured_out", &self.captured_out)
            .field("output_observer", &self.output_observer.is_some())
            .field("pid", &self.pid)
            .field("pgid", &self.pgid)
            .field("process_count", &self.process_count)
            .finish()
    }
}

impl Context {
    /// Writes `msg` followed by exactly one `\n` (via `writeln!`). `msg`
    /// itself must not end in `\n` - callers that append their own trailing
    /// newline before calling this end up with a doubled blank line, which is
    /// the bug the `dsh/src/cron` "newline sweep" fixed at every call site
    /// that used to do this.
    pub fn write_stdout(&self, msg: &str) -> Result<()> {
        if let Some(observer) = &self.output_observer
            && let Ok(mut observer) = observer.lock()
        {
            observer.append(observed_output::ObservedStream::Stdout, msg);
            observer.append(observed_output::ObservedStream::Stdout, "\n");
        }
        let mut file = unsafe { File::from_raw_fd(self.outfile) };
        writeln!(&mut file, "{msg}")?;
        mem::forget(file);
        Ok(())
    }

    pub fn write_stderr(&self, msg: &str) -> Result<()> {
        if let Some(observer) = &self.output_observer
            && let Ok(mut observer) = observer.lock()
        {
            observer.append(observed_output::ObservedStream::Stderr, msg);
            observer.append(observed_output::ObservedStream::Stderr, "\n");
        }
        let mut file = unsafe { File::from_raw_fd(self.errfile) };
        writeln!(&mut file, "{msg}")?;
        mem::forget(file);
        Ok(())
    }

    pub fn reset(&mut self) {
        self.infile = STDIN_FILENO;
        self.outfile = STDOUT_FILENO;
        self.errfile = STDERR_FILENO;
        self.captured_out = None;
        self.output_observer = None;
        self.pid = None;
        self.pgid = None;
        self.process_count = 0;
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ExitStatus {
    ExitedWith(i32),
    Running(Pid),
    Break,
    Continue,
    Return,
}

#[cfg(test)]
mod context_job_control_tests {
    use super::*;

    fn tty_capable_context(interactive: bool) -> Context {
        Context {
            shell_pid: Pid::from_raw(100),
            shell_pgid: Pid::from_raw(100),
            shell_tmode: None,
            terminal_state: TerminalState {
                is_terminal: true,
                tmodes: None,
                supports_job_control: true,
            },
            shell_mode: ShellMode::Interactive,
            foreground: false,
            interactive,
            infile: STDIN_FILENO,
            outfile: STDOUT_FILENO,
            errfile: STDERR_FILENO,
            captured_out: None,
            output_observer: None,
            save_history: false,
            pid: None,
            pgid: None,
            process_count: 0,
        }
    }

    #[test]
    fn noninteractive_tty_capable_context_disables_job_control() {
        // `dogesh -c` attached to a TTY: capability is present but the
        // execution context is not interactive, so job control stays off
        // and async helpers default their stdin to `/dev/null`.
        assert!(!tty_capable_context(false).supports_job_control());
    }

    #[test]
    fn interactive_tty_capable_context_enables_job_control() {
        assert!(tty_capable_context(true).supports_job_control());
    }
}
