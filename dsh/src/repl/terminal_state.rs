use crossterm::terminal::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled};
use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
use std::os::fd::BorrowedFd;

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

/// While a chat waits in cooked mode, let its Ctrl-C handler receive SIGINT
/// even if the shell inherited a terminal with signal keys disabled.
pub(crate) struct ChatSignalKeys<'fd> {
    input: BorrowedFd<'fd>,
    restore_disabled: bool,
}

impl<'fd> ChatSignalKeys<'fd> {
    pub(crate) fn enable(input: BorrowedFd<'fd>) -> Self {
        let restore_disabled = Self::set_enabled(input, true).unwrap_or_else(|err| {
            tracing::warn!("could not enable terminal signal keys for chat: {err}");
            false
        });
        Self {
            input,
            restore_disabled,
        }
    }

    fn set_enabled(input: BorrowedFd<'_>, enabled: bool) -> nix::Result<bool> {
        let mut mode = tcgetattr(input)?;
        if mode.local_flags.contains(LocalFlags::ISIG) == enabled {
            return Ok(false);
        }
        mode.local_flags.set(LocalFlags::ISIG, enabled);
        tcsetattr(input, SetArg::TCSANOW, &mode)?;
        Ok(true)
    }
}

impl Drop for ChatSignalKeys<'_> {
    fn drop(&mut self) {
        if self.restore_disabled
            && let Err(err) = Self::set_enabled(self.input, false)
        {
            tracing::warn!("could not restore terminal signal keys after chat: {err}");
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
    use std::os::fd::AsFd;

    #[test]
    fn chat_enables_signal_keys_for_the_turn_and_restores_only_that_bit() {
        let pty = nix::pty::openpty(None, None).unwrap();
        for initially_enabled in [false, true] {
            let mut entry = tcgetattr(&pty.slave).unwrap();
            entry.local_flags.set(LocalFlags::ISIG, initially_enabled);
            entry.local_flags.insert(LocalFlags::ECHO);
            tcsetattr(&pty.slave, SetArg::TCSANOW, &entry).unwrap();
            {
                let _keys = ChatSignalKeys::enable(pty.slave.as_fd());
                let mut during = tcgetattr(&pty.slave).unwrap();
                assert!(during.local_flags.contains(LocalFlags::ISIG));
                during.local_flags.remove(LocalFlags::ECHO);
                tcsetattr(&pty.slave, SetArg::TCSANOW, &during).unwrap();
            }
            let flags = tcgetattr(&pty.slave).unwrap().local_flags;
            assert_eq!(flags.contains(LocalFlags::ISIG), initially_enabled);
            assert!(!flags.contains(LocalFlags::ECHO));
        }
    }

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
