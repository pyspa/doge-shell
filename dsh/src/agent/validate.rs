//! The checks a task must pass before it is allowed to run at all.
//!
//! Shared by [`super::run_task`] (which always runs them) and `agent run
//! --detach`'s parent-side `start()` (`dsh/src/agent/detach.rs`), which runs
//! them a second time *before* spawning the detached child so a doomed run -
//! an empty goal, an exhausted budget, a root that no longer resolves -
//! fails in front of the person who typed the command instead of silently
//! inside a process nobody is watching.

use super::{SqliteTaskStore, TaskFailure, setting, tag_failure};
use anyhow::Result;
use dsh_types::agent::AgentTask;

/// Checks a task is in a state [`super::run_task`] can actually start from.
///
/// A pure check except for one side effect it cannot avoid: recording any
/// sensitive-looking value from the task's `--env` grant into `store`'s
/// redaction set, which has to happen before anything using that value can
/// be logged - including a failed `startable` call itself.
pub(crate) fn startable(
    shell: &mut crate::shell::Shell,
    store: &SqliteTaskStore,
    task: &AgentTask,
    reconcile: Option<&str>,
) -> Result<()> {
    for path in task
        .grant
        .read_roots
        .iter()
        .chain(&task.grant.write_roots)
        .chain(std::iter::once(&task.root))
    {
        let canonical = path.canonicalize().map_err(|error| {
            anyhow::Error::new(TaskFailure::RootChanged)
                .context(format!("task root no longer resolves: {error}"))
        })?;
        if canonical != *path {
            return Err(anyhow::Error::new(TaskFailure::RootChanged)
                .context("task root changed identity; inspect and start a new task"));
        }
    }
    if task.grant.sandbox {
        dsh_builtin::agent::sandbox::find_runtime()
            .map_err(|error| tag_failure(TaskFailure::Config, error))?;
    }
    for name in &task.grant.environment {
        if dsh_types::safety_policy::is_sensitive_key(name)
            && let Some(value) = setting(shell, name)
            && value.len() >= 4
        {
            store.secrets.lock().push(value);
        }
    }
    if task.goal.trim().is_empty() {
        return Err(anyhow::Error::new(TaskFailure::Config).context("goal required after --"));
    }
    if task.token_budget <= task.tokens_used || task.time_budget_ms <= task.elapsed_ms {
        return Err(anyhow::Error::new(TaskFailure::Config).context(
            "positive remaining token and time budgets are required \
             (defaults 50000 tokens / 900s; see --tokens/--timeout or \
             AI_AGENT_TOKEN_BUDGET/AI_AGENT_TIMEOUT_SECS)",
        ));
    }
    if task.pending_operation.is_some() && reconcile.is_none() {
        return Err(anyhow::Error::new(TaskFailure::Reconcile).context(format!(
            "previous operation has an unknown outcome; inspect `agent show {}` and the actual files/service, then resume with --reconcile describing the observed result",
            task.id
        )));
    }
    Ok(())
}
