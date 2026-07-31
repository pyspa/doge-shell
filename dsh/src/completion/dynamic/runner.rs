use anyhow::Result;
use std::process::Command;
use std::time::Duration;

use super::super::subprocess;

const COMMAND_TIMEOUT: Duration = Duration::from_millis(1500);

pub(super) fn command(program: &str) -> Command {
    subprocess::command(program)
}

pub(super) fn shell_command(template: &str) -> Command {
    subprocess::shell_command(template)
}

pub(super) fn collect_stdout(command: Command) -> Result<String> {
    subprocess::collect_stdout(command, COMMAND_TIMEOUT)
}

pub(super) const fn timeout() -> Duration {
    COMMAND_TIMEOUT
}
