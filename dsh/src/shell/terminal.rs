use nix::sys::termios::{SetArg, Termios, tcgetattr, tcsetattr};

pub const SHELL_TERMINAL: std::os::raw::c_int = std::os::unix::io::RawFd::MAX; // Placeholder, likely needs fixing or imports

use libc::STDIN_FILENO;

// We need to import SHELL_TERMINAL from somewhere or define it.
// In shell.rs it was: pub const SHELL_TERMINAL: c_int = STDIN_FILENO;
// We can define it here.

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
        let original_termios = tcgetattr(SHELL_TERMINAL_FD).ok();
        Self { original_termios }
    }

    pub fn restore(&mut self) {
        if let Some(termios) = self.original_termios.take() {
            let _ = tcsetattr(SHELL_TERMINAL_FD, SetArg::TCSADRAIN, &termios);
        }
    }
}

impl Drop for RawModeRestore {
    fn drop(&mut self) {
        self.restore();
    }
}
