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
use thiserror::Error;

pub mod terminal;
pub use terminal::{ShellMode, TerminalState};

/// Doge Shell specific error types
#[derive(Error, Debug)]
pub enum DshError {
    #[error("IO operation failed: {0}")]
    Io(#[from] std::io::Error),

    #[error("Process execution failed: {message}")]
    Process { message: String },

    #[error("File operation failed: {operation} on {path}: {source}")]
    File {
        operation: String,
        path: String,
        source: std::io::Error,
    },

    #[error("History operation failed: {0}")]
    History(String),

    #[error("Lisp evaluation failed: {0}")]
    Lisp(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Lock operation failed: {0}")]
    Lock(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("System call failed: {0}")]
    System(String),
}

pub type DshResult<T> = std::result::Result<T, DshError>;

#[derive(Clone)]
pub struct Context {
    pub shell_pid: Pid,
    pub shell_pgid: Pid,
    pub shell_tmode: Termios,
    pub terminal_state: TerminalState,
    pub shell_mode: ShellMode,
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
            save_history: true,
            pid: None,
            pgid: None,
            process_count: 0,
        }
    }

    /// 安全なContext作成（ターミナル検出付き）
    pub fn new_safe(shell_pid: Pid, shell_pgid: Pid, foreground: bool) -> Self {
        let terminal_state = TerminalState::detect(STDIN_FILENO);
        let shell_mode = ShellMode::detect();

        // ターミナル設定を安全に取得
        let shell_tmode = terminal_state.get_tmodes().cloned().unwrap_or_else(|| {
            // デフォルトのTermios値を作成
            // 実際の実装では適切なデフォルト値を設定
            unsafe { std::mem::zeroed() }
        });

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
            save_history: true,
            pid: None,
            pgid: None,
            process_count: 0,
        }
    }

    /// ジョブ制御がサポートされているかチェック
    pub fn supports_job_control(&self) -> bool {
        self.terminal_state.supports_job_control && self.shell_mode.supports_job_control()
    }

    /// 対話的モードかチェック
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
