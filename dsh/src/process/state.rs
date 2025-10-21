use nix::sys::signal::Signal;
use nix::unistd::Pid;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ProcessState {
    Running,
    Completed(u8, Option<Signal>),
    Stopped(Pid, Signal),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubshellType {
    None,
    Subshell,
    ProcessSubstitution,
    CommandSubstitution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListOp {
    None,
    And,
    Or,
}
