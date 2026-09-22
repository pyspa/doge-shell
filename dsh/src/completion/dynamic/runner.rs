use anyhow::Result;
use std::process::Command;
use std::time::Duration;

use super::super::subprocess;
use super::super::subprocess::CollectStdoutOutcome;

const COMMAND_TIMEOUT: Duration = Duration::from_millis(1500);
pub(super) const REMOTE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) fn command(program: &str) -> Command {
    subprocess::command(program)
}

pub(super) fn shell_command(template: &str) -> Command {
    subprocess::shell_command(template)
}

pub(super) fn collect_stdout(command: Command) -> Result<String> {
    collect_stdout_for_completion(command, COMMAND_TIMEOUT)
}

pub(super) fn collect_stdout_with_timeout(command: Command, timeout: Duration) -> Result<String> {
    collect_stdout_for_completion(command, timeout)
}

fn collect_stdout_for_completion(command: Command, timeout: Duration) -> Result<String> {
    let outcome = subprocess::collect_stdout_outcome(command, timeout)?;
    map_collect_outcome(outcome)
}

fn map_collect_outcome(outcome: CollectStdoutOutcome) -> Result<String> {
    match outcome {
        CollectStdoutOutcome::Completed(stdout) => Ok(stdout),
        // Non-zero exit stays soft-empty for compatibility: some providers
        // signal "no values in this context" via exit status.
        CollectStdoutOutcome::NonZeroExit { .. } => Ok(String::new()),
        CollectStdoutOutcome::TimedOut { timeout } => {
            anyhow::bail!(
                "completion subprocess timed out after {}ms",
                timeout.as_millis()
            )
        }
        CollectStdoutOutcome::OutputLimitExceeded { limit } => {
            anyhow::bail!("completion subprocess exceeded stdout limit of {limit} bytes")
        }
    }
}

pub(super) const fn timeout() -> Duration {
    COMMAND_TIMEOUT
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nonzero_exit() -> CollectStdoutOutcome {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 7")
            .status()
            .expect("sh should run");
        assert_eq!(status.code(), Some(7));
        CollectStdoutOutcome::NonZeroExit { status }
    }

    #[test]
    fn completed_stdout_passes_through() {
        assert_eq!(
            map_collect_outcome(CollectStdoutOutcome::Completed("api".to_string())).unwrap(),
            "api"
        );
    }

    #[test]
    fn nonzero_exit_maps_to_empty_for_compatibility() {
        assert_eq!(map_collect_outcome(nonzero_exit()).unwrap(), "");
    }

    #[test]
    fn timeout_maps_to_error() {
        let err = map_collect_outcome(CollectStdoutOutcome::TimedOut {
            timeout: Duration::from_millis(1500),
        })
        .expect_err("timeout must be an error");
        assert!(
            err.to_string().contains("timed out"),
            "unexpected timeout message: {err}"
        );
    }

    #[test]
    fn output_limit_maps_to_error() {
        let err = map_collect_outcome(CollectStdoutOutcome::OutputLimitExceeded { limit: 1024 })
            .expect_err("output limit must be an error");
        assert!(
            err.to_string().contains("stdout limit"),
            "unexpected limit message: {err}"
        );
    }
}
