use nix::sys::termios::{SetArg, Termios, tcgetattr, tcsetattr};
use std::os::fd::BorrowedFd;

pub const SHELL_TERMINAL: std::os::raw::c_int = std::os::unix::io::RawFd::MAX;

use libc::STDIN_FILENO;

pub const SHELL_TERMINAL_FD: i32 = STDIN_FILENO;

pub struct RawModeRestore {
    original_termios: Option<Termios>,
}

impl Default for RawModeRestore {
    fn default() -> Self {
        Self::new()
    }
}

impl RawModeRestore {
    pub fn new() -> Self {
        let original_termios = tcgetattr(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL_FD) }).ok();
        Self { original_termios }
    }

    pub fn restore(&mut self) {
        if let Some(termios) = self.original_termios.take() {
            let _ = tcsetattr(
                unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL_FD) },
                SetArg::TCSADRAIN,
                &termios,
            );
        }
    }
}

impl Drop for RawModeRestore {
    fn drop(&mut self) {
        self.restore();
    }
}
