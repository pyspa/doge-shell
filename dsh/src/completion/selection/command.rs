//! Bounded legacy command completion and an input-preserving asynchronous wait.
use super::{
    CompletionFrameworkKind, CompletionSelection, select_completion_items,
    select_completion_items_simple,
};
use crate::completion::{Candidate, subprocess};
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use dsh_types::process_runtime::CommandRuntimeSnapshot;
use futures::{Stream, StreamExt};
use std::io;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::task::JoinHandle;

const COMMAND_TIMEOUT: Duration = Duration::from_millis(1500);
type CollectionResult = Result<Option<Vec<Candidate>>>;

pub(super) fn complete_sync(
    input: String,
    query: Option<&str>,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    // This compatibility API has no Environment parameter. Runtime callers
    // must use complete_with_runtime so child variables are logical snapshots.
    match collect_candidates(subprocess::shell_command(&input), &|| false) {
        Ok(Some(items)) => select_completion_items_simple(items, query, framework),
        Ok(None) => CompletionSelection::Cancelled,
        Err(error) => {
            tracing::debug!("Legacy completion helper failed: {error}");
            CompletionSelection::None
        }
    }
}

pub(super) async fn complete_with_runtime(
    input: String,
    runtime: CommandRuntimeSnapshot,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    let task = CommandTask::start(runtime_command(&input, &runtime));
    // The caller is a key handler awaited by the REPL, so its EventStream is
    // not polled concurrently. Never construct a terminal reader in unit tests
    // or when stdin is a pipe (same ownership contract as ai_features::await_ui).
    let result = if interactive() {
        wait_for_command(task, EventStream::new()).await
    } else {
        wait_for_command(task, futures::stream::pending::<io::Result<Event>>()).await
    };
    match result {
        CommandResult::Candidates(items) => {
            select_completion_items(items, query, prompt_text, input_text, framework)
        }
        CommandResult::Interrupted(selection) => selection,
    }
}

fn runtime_command(input: &str, runtime: &CommandRuntimeSnapshot) -> Command {
    let mut command = subprocess::shell_command(input);
    command
        .env_clear()
        .envs(runtime.child_env())
        .current_dir(runtime.current_dir());
    command
}

fn collect_candidates(command: Command, cancelled: &dyn Fn() -> bool) -> CollectionResult {
    use subprocess::CollectStdoutOutcome;
    let Some(outcome) =
        subprocess::collect_stdout_outcome_cancellable(command, COMMAND_TIMEOUT, cancelled)?
    else {
        return Ok(None);
    };
    match outcome {
        CollectStdoutOutcome::Completed(stdout) => Ok(Some(
            stdout
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| Candidate::Basic(line.to_string()))
                .collect(),
        )),
        CollectStdoutOutcome::NonZeroExit { .. } => Ok(Some(Vec::new())),
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

struct CommandTask {
    cancelled: Arc<AtomicBool>,
    handle: JoinHandle<CollectionResult>,
}

impl CommandTask {
    fn start(command: Command) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let handle = tokio::task::spawn_blocking(move || {
            collect_candidates(command, &|| worker_cancelled.load(Ordering::Acquire))
        });
        Self { cancelled, handle }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl Drop for CommandTask {
    fn drop(&mut self) {
        // Dropping an awaited future must also stop its blocking worker.
        self.cancel();
    }
}

enum CommandResult {
    Candidates(Vec<Candidate>),
    Interrupted(CompletionSelection),
}

async fn wait_for_command<S>(mut task: CommandTask, mut events: S) -> CommandResult
where
    S: Stream<Item = io::Result<Event>> + Unpin,
{
    loop {
        tokio::select! {
            biased;
            event = events.next() => {
                let selection = match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Release => continue,
                    Some(Ok(event)) if is_cancel(&event) => CompletionSelection::Cancelled,
                    Some(Ok(event)) => CompletionSelection::Event(event),
                    Some(Err(_)) => CompletionSelection::Cancelled,
                    None => return finish_task((&mut task.handle).await),
                };
                task.cancel();
                // The worker checks cancellation between nonblocking reads and
                // polls, kills the group, and reaps before the editor resumes.
                let _ = (&mut task.handle).await;
                return CommandResult::Interrupted(selection);
            }
            result = &mut task.handle => return finish_task(result),
        }
    }
}

fn finish_task(
    result: std::result::Result<CollectionResult, tokio::task::JoinError>,
) -> CommandResult {
    match result {
        Ok(Ok(Some(items))) => CommandResult::Candidates(items),
        Ok(Ok(None)) => CommandResult::Interrupted(CompletionSelection::Cancelled),
        Ok(Err(error)) => {
            tracing::debug!("Legacy completion helper failed: {error}");
            CommandResult::Candidates(Vec::new())
        }
        Err(error) => {
            tracing::debug!("Legacy completion worker failed: {error}");
            CommandResult::Candidates(Vec::new())
        }
    }
}

fn interactive() -> bool {
    use std::io::IsTerminal;
    crate::terminal::terminal_control_enabled() && std::io::stdin().is_terminal()
}

fn is_cancel(event: &Event) -> bool {
    let Event::Key(key) = event else { return false };
    key.kind != KeyEventKind::Release
        && match key.code {
            KeyCode::Esc => true,
            KeyCode::Char('c' | 'C') => key.modifiers.contains(KeyModifiers::CONTROL),
            _ => false,
        }
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
