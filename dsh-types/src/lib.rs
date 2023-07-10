use anyhow::Result;
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::termios::Termios;
use nix::unistd::Pid;
use std::fmt::Debug;
use std::fs::File;
use std::io::Write;
use std::mem;
use std::os::unix::io::FromRawFd;
use std::os::unix::io::RawFd;

#[derive(Clone)]
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
    pub pid: Option<Pid>,
    pub pgid: Option<Pid>,
    pub process_count: u32,
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
            pid: None,
            pgid: None,
            process_count: 0,
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
            .field("pid", &self.pid)
            .field("pgid", &self.pgid)
            .field("process_count", &self.process_count)
            .finish()
    }
}

impl Context {
    pub fn write_stdout(&self, msg: &str) -> Result<()> {
        let mut file = unsafe { File::from_raw_fd(self.outfile) };
        writeln!(&mut file, "{}", msg)?;
        mem::forget(file);
        Ok(())
    }

    pub fn write_stderr(&self, msg: &str) -> Result<()> {
        let mut file = unsafe { File::from_raw_fd(self.errfile) };
        writeln!(&mut file, "{}", msg)?;
        mem::forget(file);
        Ok(())
    }

    pub fn reset(&mut self) {
        self.infile = STDIN_FILENO;
        self.outfile = STDOUT_FILENO;
        self.errfile = STDERR_FILENO;
        self.captured_out = None;
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
