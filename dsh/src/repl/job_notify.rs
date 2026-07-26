//! Formatting for asynchronous job status notices (`[1]+  Done  sleep 5`).
//!
//! The formatter takes a plain owned struct rather than `&Job` on purpose: a
//! real `Job` requires a live process, which makes unit testing impossible.

use crate::process::state::ProcessState;
use nix::sys::signal::Signal;

/// Width of the state column, matching bash's job table layout.
const STATE_WIDTH: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobNoticeState {
    Done,
    Exit(u8),
    Terminated,
    Killed,
    Stopped,
}

impl JobNoticeState {
    fn label(&self) -> String {
        match self {
            JobNoticeState::Done => "Done".to_string(),
            JobNoticeState::Exit(code) => format!("Exit {}", code),
            JobNoticeState::Terminated => "Terminated".to_string(),
            JobNoticeState::Killed => "Killed".to_string(),
            JobNoticeState::Stopped => "Stopped".to_string(),
        }
    }

    /// Exit status a notification should report, using the shell convention of
    /// `128 + signal` for a signalled job.
    ///
    /// Derived from the same state as the on-screen notice so the two can never
    /// disagree — reading the raw `Completed(code, _)` would call a job killed
    /// by SIGKILL a success whenever its recorded code happens to be zero.
    pub(crate) fn exit_code(&self) -> i32 {
        match self {
            JobNoticeState::Done => 0,
            JobNoticeState::Exit(code) => i32::from(*code),
            JobNoticeState::Terminated => 128 + Signal::SIGTERM as i32,
            JobNoticeState::Killed => 128 + Signal::SIGKILL as i32,
            // Stopped jobs are not reported as finished.
            JobNoticeState::Stopped => 0,
        }
    }
}

/// `+` marks the current job, `-` the previous one, space for the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobMarker {
    Current,
    Previous,
    None,
}

impl JobMarker {
    /// Marker for the nth job in a batch of notices, most recent first.
    pub(crate) fn for_index(index: usize) -> Self {
        match index {
            0 => JobMarker::Current,
            1 => JobMarker::Previous,
            _ => JobMarker::None,
        }
    }

    fn glyph(&self) -> char {
        match self {
            JobMarker::Current => '+',
            JobMarker::Previous => '-',
            JobMarker::None => ' ',
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct JobNotice {
    pub job_id: usize,
    pub cmd: String,
    pub state: JobNoticeState,
    pub marker: JobMarker,
}

/// Map a `ProcessState` to the notice state.
///
/// The signal mapping mirrors `ProcessState`'s `Display` impl so the two
/// cannot drift apart.
pub(crate) fn notice_state_from(state: &ProcessState) -> JobNoticeState {
    match state {
        ProcessState::Running => JobNoticeState::Done,
        ProcessState::Stopped(_, _) => JobNoticeState::Stopped,
        ProcessState::Completed(code, signal) => match signal {
            Some(Signal::SIGKILL) => JobNoticeState::Killed,
            Some(Signal::SIGTERM) => JobNoticeState::Terminated,
            _ if *code == 0 => JobNoticeState::Done,
            _ => JobNoticeState::Exit(*code),
        },
    }
}

/// Render a bash-compatible job notice line, e.g. `[1]+  Done   sleep 5`.
///
/// Newlines in the command are flattened so that a pasted multi-line command
/// cannot change the emitted line count — `print_above_prompt` computes cursor
/// movement from `lines.len()`.
pub(crate) fn format_job_notice(notice: &JobNotice) -> String {
    let cmd = flatten_command(&notice.cmd);
    format!(
        "[{}]{}  {:<width$}{}",
        notice.job_id,
        notice.marker.glyph(),
        notice.state.label(),
        cmd,
        width = STATE_WIDTH
    )
}

fn flatten_command(cmd: &str) -> String {
    cmd.replace("\r\n", "⏎").replace(['\n', '\r'], "⏎")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::Pid;

    fn notice(state: JobNoticeState) -> JobNotice {
        JobNotice {
            job_id: 1,
            cmd: "sleep 5".to_string(),
            state,
            marker: JobMarker::Current,
        }
    }

    #[test]
    fn format_job_notice_done() {
        assert_eq!(
            format_job_notice(&notice(JobNoticeState::Done)),
            "[1]+  Done                    sleep 5"
        );
    }

    #[test]
    fn format_job_notice_exit_code() {
        assert_eq!(
            format_job_notice(&notice(JobNoticeState::Exit(2))),
            "[1]+  Exit 2                  sleep 5"
        );
    }

    #[test]
    fn format_job_notice_killed() {
        assert!(format_job_notice(&notice(JobNoticeState::Killed)).contains("Killed"));
    }

    #[test]
    fn format_job_notice_terminated() {
        assert!(format_job_notice(&notice(JobNoticeState::Terminated)).contains("Terminated"));
    }

    #[test]
    fn format_job_notice_stopped() {
        assert!(format_job_notice(&notice(JobNoticeState::Stopped)).contains("Stopped"));
    }

    #[test]
    fn format_job_notice_marker_plus_and_minus() {
        let mut n = notice(JobNoticeState::Done);
        n.marker = JobMarker::Current;
        assert!(format_job_notice(&n).starts_with("[1]+"));
        n.marker = JobMarker::Previous;
        assert!(format_job_notice(&n).starts_with("[1]-"));
        n.marker = JobMarker::None;
        assert!(format_job_notice(&n).starts_with("[1] "));
    }

    #[test]
    fn format_job_notice_pads_state_column() {
        // Command always starts at the same column regardless of state length.
        let done = format_job_notice(&notice(JobNoticeState::Done));
        let killed = format_job_notice(&notice(JobNoticeState::Killed));
        assert_eq!(
            done.find("sleep 5").unwrap(),
            killed.find("sleep 5").unwrap()
        );
    }

    #[test]
    fn format_job_notice_flattens_multiline_command() {
        let n = JobNotice {
            job_id: 3,
            cmd: "echo a\necho b\r\necho c".to_string(),
            state: JobNoticeState::Done,
            marker: JobMarker::None,
        };
        let out = format_job_notice(&n);
        assert!(!out.contains('\n'));
        assert!(!out.contains('\r'));
        assert!(out.contains("echo a⏎echo b⏎echo c"));
    }

    #[test]
    fn notice_state_from_maps_sigkill_to_killed() {
        assert_eq!(
            notice_state_from(&ProcessState::Completed(137, Some(Signal::SIGKILL))),
            JobNoticeState::Killed
        );
        assert_eq!(
            notice_state_from(&ProcessState::Completed(143, Some(Signal::SIGTERM))),
            JobNoticeState::Terminated
        );
        assert_eq!(
            notice_state_from(&ProcessState::Completed(0, None)),
            JobNoticeState::Done
        );
        assert_eq!(
            notice_state_from(&ProcessState::Completed(2, None)),
            JobNoticeState::Exit(2)
        );
        assert_eq!(
            notice_state_from(&ProcessState::Stopped(Pid::from_raw(1), Signal::SIGTSTP)),
            JobNoticeState::Stopped
        );
    }

    #[test]
    fn job_marker_for_index() {
        assert_eq!(JobMarker::for_index(0), JobMarker::Current);
        assert_eq!(JobMarker::for_index(1), JobMarker::Previous);
        assert_eq!(JobMarker::for_index(2), JobMarker::None);
    }
}
