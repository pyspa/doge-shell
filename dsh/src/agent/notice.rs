//! Rendering a one-line notice for a detached task that just stopped -
//! `[agent 1a2b3c4d]  Completed  the task's own goal`.
//!
//! Deliberately the same shape as `dsh/src/repl/job_notify.rs`'s job notices
//! (same state-column width, same newline flattening): a person who already
//! reads `[1]+  Done  sleep 5` above their prompt should read this as the
//! same kind of thing, not a different notification system.

use super::{blocked, summary};
use crate::repl::job_notify::{STATE_WIDTH, flatten_command};
use dsh_types::agent::{AgentTask, TaskStatus};

/// Characters of a task id shown in a notice - enough to tell tasks apart at
/// a glance, matched against with a prefix in `agent show`/`agent logs`
/// error messages the same way short git hashes are.
const SHORT_ID_CHARS: usize = 8;

fn short_id(id: &str) -> &str {
    let cut = id
        .char_indices()
        .nth(SHORT_ID_CHARS)
        .map_or(id.len(), |(i, _)| i);
    &id[..cut]
}

/// One line, e.g.:
///
/// ```text
/// [agent 1a2b3c4d]  Completed               fix the failing test
/// [agent 1a2b3c4d]  Needs approval           agent resume 1a2b3c4d --allow-command 'cargo test'
/// ```
///
/// For `InputRequired` with a known fix, the body names the exact command to
/// run rather than repeating the reason - the reason is already one `agent
/// show` away, and the point of a notice a person did not ask for is to say
/// what to do about it, not to restate what happened.
pub(crate) fn render(task: &AgentTask) -> String {
    let (label, body) = if task.status == TaskStatus::InputRequired {
        match blocked::blocked_need(task).and_then(|need| need.fix) {
            Some(fix) => ("Needs approval".to_string(), fix),
            None => (
                summary::status_label(task.status).to_string(),
                flatten_command(&task.goal),
            ),
        }
    } else {
        (
            summary::status_label(task.status).to_string(),
            flatten_command(&task.goal),
        )
    };
    format!(
        "[agent {}]  {:<width$}{}",
        short_id(&task.id),
        label,
        body,
        width = STATE_WIDTH
    )
}

#[cfg(test)]
mod tests;
