use crossterm::terminal::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled};

/// Temporarily releases raw mode and restores the previous terminal state on
/// every return path.
pub(crate) struct RawModePause {
    restore_raw_mode: bool,
}

impl RawModePause {
    pub fn new() -> Self {
        let restore_raw_mode = is_raw_mode_enabled().unwrap_or(false);
        if restore_raw_mode {
            disable_raw_mode().ok();
        }
        Self { restore_raw_mode }
    }
}

impl Drop for RawModePause {
    fn drop(&mut self) {
        if self.restore_raw_mode {
            enable_raw_mode().ok();
        }
    }
}

/// Puts raw mode back the way it was found, on every return path.
///
/// The inverse of [`RawModePause`]: wrap code that toggles raw mode *on* so it
/// cannot leak. `eval_str` needs this because crossterm keeps the saved
/// pre-raw termios in a process-global, so a process that exits while raw
/// leaves the terminal raw with no way to recover it.
pub(crate) struct RawModeRestore {
    was_raw: bool,
}

impl RawModeRestore {
    pub fn new() -> Self {
        Self {
            was_raw: is_raw_mode_enabled().unwrap_or(false),
        }
    }
}

impl Drop for RawModeRestore {
    fn drop(&mut self) {
        let is_raw = is_raw_mode_enabled().unwrap_or(false);
        if self.was_raw && !is_raw {
            enable_raw_mode().ok();
        } else if !self.was_raw && is_raw {
            disable_raw_mode().ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_is_safe_when_raw_mode_is_not_enabled() {
        let pause = RawModePause::new();
        drop(pause);
    }

    #[test]
    fn restore_is_a_no_op_when_raw_mode_stays_off() {
        let guard = RawModeRestore::new();
        assert!(!guard.was_raw);
        drop(guard);
        assert!(!is_raw_mode_enabled().unwrap_or(false));
    }
}
