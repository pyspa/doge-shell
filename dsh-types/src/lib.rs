use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::termios::Termios;
use nix::unistd::Pid;
use std::fmt::Debug;
use std::os::unix::io::RawFd;

pub struct Context {
    pub shell_pid: Pid,
    pub shell_pgid: Pid,
    pub shell_tmode: Termios,
    pub foreground: bool,
    pub interactive: bool,
    pub infile: RawFd,
    pub outfile: RawFd,
    pub errfile: RawFd,
    pub captured_out: Option<RawFd>,
    pub save_history: bool,
}

impl Context {
    pub fn new(shell_pid: Pid, shell_pgid: Pid, shell_tmode: Termios, foreground: bool) -> Self {
        Context {
            shell_pid,
            shell_pgid,
            shell_tmode,
            foreground,
            interactive: true,
            infile: STDIN_FILENO,
            outfile: STDOUT_FILENO,
            errfile: STDERR_FILENO,
            captured_out: None,
            save_history: true,
        }
    }
}

impl Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("Context")
            .field("shell_pid", &self.shell_pid)
            .field("shell_pgid", &self.shell_pgid)
            .field("foreground", &self.foreground)
            .field("interactive", &self.interactive)
            .field("infile", &self.infile)
            .field("outfile", &self.outfile)
            .field("errfile", &self.errfile)
            .field("captured_out", &self.captured_out)
            .finish()
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
