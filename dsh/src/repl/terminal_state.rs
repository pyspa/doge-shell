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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_is_safe_when_raw_mode_is_not_enabled() {
        let pause = RawModePause::new();
        drop(pause);
    }
}
