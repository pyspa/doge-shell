//! First-Tab dynamic collection with bounded, input-preserving waiting.
use crate::completion::integrated::{CompletionDisposition, CompletionResult};
use crate::repl::{Repl, state::ReplControlFlow};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::{Stream, StreamExt};
use std::{future::Future, io, io::IsTerminal, path::Path, time::Duration};

pub(super) async fn collect(
    repl: &Repl<'_>,
    input: &str,
    cursor: usize,
    cwd: &Path,
) -> Result<CompletionResult, ReplControlFlow> {
    let engine = &repl.completion_ui.integrated_completion;
    let runtime = engine.runtime();
    let mut refresh = runtime.subscribe_refresh();
    let first = engine
        .complete(
            input,
            cursor,
            cwd,
            crate::completion::MAX_RESULT,
            repl.shell.cmd_history.as_ref(),
        )
        .await;
    if first.disposition != CompletionDisposition::Pending {
        return Ok(first);
    }
    let collection = async {
        loop {
            // Independent of the REPL notifier: do not consume its redraws.
            let _ = refresh.changed().await;
            let result = engine
                .complete(
                    input,
                    cursor,
                    cwd,
                    crate::completion::MAX_RESULT,
                    repl.shell.cmd_history.as_ref(),
                )
                .await;
            if result.disposition != CompletionDisposition::Pending {
                return result;
            }
        }
    };
    // As with legacy command completion, the outer REPL reader is not polled
    // while this key handler is awaited. Tests must never read the real tty.
    if crate::terminal::terminal_control_enabled() && io::stdin().is_terminal() {
        wait(collection, EventStream::new(), Duration::from_secs(2)).await
    } else {
        wait(
            collection,
            futures::stream::pending(),
            Duration::from_secs(2),
        )
        .await
    }
}

async fn wait<T>(
    collection: impl Future<Output = T>,
    mut events: impl Stream<Item = io::Result<Event>> + Unpin,
    budget: Duration,
) -> Result<T, ReplControlFlow> {
    tokio::pin!(collection);
    let deadline = tokio::time::sleep(budget);
    tokio::pin!(deadline);
    let mut events_closed = false;
    loop {
        tokio::select! {
            biased;
            event = events.next(), if !events_closed => match event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Release => {},
                Some(Ok(Event::Key(key))) if key.code == KeyCode::Esc
                    || (matches!(key.code, KeyCode::Char('c' | 'C'))
                        && key.modifiers.contains(KeyModifiers::CONTROL)) => {
                    return Err(ReplControlFlow::Continue);
                }
                Some(Ok(event)) => return Err(ReplControlFlow::ReplayInput(event)),
                Some(Err(_)) => return Err(ReplControlFlow::Continue),
                None => events_closed = true,
            },
            result = &mut collection => return Ok(result),
            _ = &mut deadline => return Err(ReplControlFlow::Continue),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[tokio::test]
    async fn editing_paste_and_resize_interrupt_wait_without_losing_event() {
        for event in [
            Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            Event::Paste("ssh.service".into()),
            Event::Resize(100, 40),
        ] {
            let result = wait(
                std::future::pending::<()>(),
                futures::stream::iter([Ok(event.clone())]),
                Duration::from_secs(1),
            )
            .await;
            assert!(matches!(result, Err(ReplControlFlow::ReplayInput(actual)) if actual == event));
        }
    }

    #[tokio::test]
    async fn cancel_and_timeout_do_not_fall_through_to_file_completion() {
        for key in [
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            let result = wait(
                std::future::pending::<()>(),
                futures::stream::iter([Ok(Event::Key(key))]),
                Duration::from_secs(1),
            )
            .await;
            assert!(matches!(result, Err(ReplControlFlow::Continue)));
        }
        let result = wait(
            std::future::pending::<()>(),
            futures::stream::pending(),
            Duration::ZERO,
        )
        .await;
        assert!(matches!(result, Err(ReplControlFlow::Continue)));
    }

    #[tokio::test]
    async fn ready_completion_survives_key_release_and_closed_input_stream() {
        let release =
            KeyEvent::new_with_kind(KeyCode::Tab, KeyModifiers::NONE, KeyEventKind::Release);
        let result = wait(
            async { "ssh.service" },
            futures::stream::iter([Ok(Event::Key(release))]),
            Duration::from_secs(1),
        )
        .await;
        assert!(matches!(result, Ok("ssh.service")));
    }
}
