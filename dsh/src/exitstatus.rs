use nix::unistd::Pid;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ExitStatus {
    ExitedWith(i32),
    Running(Pid),
    Break,
    Continue,
    Return,
}
