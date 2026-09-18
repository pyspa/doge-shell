use nix::sys::signal::Signal;
use nix::unistd::Pid;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ProcessState {
    Running,
    Completed(u8, Option<Signal>),
    Stopped(Pid, Signal),
}

/// Shell exit status for a process terminated by `signal`.
///
/// A process exiting non-zero is data, not a reason to kill its pipeline
/// siblings. A process killed by signal N has shell status 128 + N,
/// computed here in one place.
pub const fn signal_exit_status(signal: Signal) -> i32 {
    128 + signal as i32
}

impl ProcessState {
    /// Normal exit state construction.
    pub const fn exited(code: u8) -> Self {
        Self::Completed(code, None)
    }

    /// Signal exit state construction, normalizing the stored code to
    /// `128 + signal`.
    pub const fn signaled(signal: Signal) -> Self {
        Self::Completed((128 + signal as i32) as u8, Some(signal))
    }

    /// Shell exit code for this state.
    ///
    /// The signal is the authoritative source when present: even a stale
    /// `Completed(1, Some(signal))` maps to `128 + signal`.
    pub const fn shell_exit_code(&self) -> Option<i32> {
        match self {
            Self::Completed(_, Some(signal)) => Some(signal_exit_status(*signal)),
            Self::Completed(code, None) => Some(*code as i32),
            Self::Running | Self::Stopped(_, _) => None,
        }
    }
}

impl std::fmt::Display for ProcessState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ProcessState::Running => formatter.write_str("running"),
            ProcessState::Completed(_, signal) => {
                if let Some(signal) = signal {
                    if signal == &Signal::SIGKILL {
                        formatter.write_str("killed")
                    } else if signal == &Signal::SIGTERM {
                        formatter.write_str("terminated")
                    } else {
                        formatter.write_str("done")
                    }
                } else {
                    formatter.write_str("done")
                }
            }
            ProcessState::Stopped(_, _) => formatter.write_str("stopped"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SubshellType {
    None,
    Subshell,
    ProcessSubstitution,
    CommandSubstitution,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ListOp {
    None,
    And,
    Or,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_exit_maps_to_itself() {
        assert_eq!(ProcessState::exited(0).shell_exit_code(), Some(0));
        assert_eq!(ProcessState::exited(7).shell_exit_code(), Some(7));
        assert_eq!(ProcessState::Completed(0, None).shell_exit_code(), Some(0));
    }

    #[test]
    fn signal_exit_status_follows_128_plus_signal() {
        assert_eq!(signal_exit_status(Signal::SIGINT), 130);
        assert_eq!(signal_exit_status(Signal::SIGKILL), 137);
        assert_eq!(signal_exit_status(Signal::SIGTERM), 143);
        assert_eq!(signal_exit_status(Signal::SIGPIPE), 141);
        assert_eq!(
            ProcessState::signaled(Signal::SIGINT).shell_exit_code(),
            Some(130)
        );
        assert_eq!(
            ProcessState::signaled(Signal::SIGKILL).shell_exit_code(),
            Some(137)
        );
        assert_eq!(
            ProcessState::signaled(Signal::SIGTERM).shell_exit_code(),
            Some(143)
        );
        assert_eq!(
            ProcessState::signaled(Signal::SIGPIPE).shell_exit_code(),
            Some(141)
        );
    }

    #[test]
    fn signaled_constructor_normalizes_stored_code() {
        assert_eq!(
            ProcessState::signaled(Signal::SIGTERM),
            ProcessState::Completed(143, Some(Signal::SIGTERM))
        );
        assert_eq!(
            ProcessState::signaled(Signal::SIGINT),
            ProcessState::Completed(130, Some(Signal::SIGINT))
        );
    }

    #[test]
    fn signal_is_authoritative_over_stored_code() {
        // Fail-safe for stale states built before the `signaled` constructor.
        assert_eq!(
            ProcessState::Completed(1, Some(Signal::SIGTERM)).shell_exit_code(),
            Some(143)
        );
        assert_eq!(
            ProcessState::Completed(0, Some(Signal::SIGKILL)).shell_exit_code(),
            Some(137)
        );
    }

    #[test]
    fn non_completed_states_have_no_exit_code() {
        assert_eq!(ProcessState::Running.shell_exit_code(), None);
        assert_eq!(
            ProcessState::Stopped(Pid::from_raw(1), Signal::SIGTSTP).shell_exit_code(),
            None
        );
    }
}
