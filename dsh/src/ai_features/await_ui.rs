//! Waiting for a shell-side AI request without the shell going deaf.
//!
//! The REPL runs its key handlers from the same task as its `tokio::select!`
//! over terminal input, so anything it `.await`s blocks that select. Before
//! this, an `Alt+d` or a command-palette AI action meant the terminal read no
//! keys at all for up to `AI_CHAT_TIMEOUT_SECS` - and `cancel_requests`, which
//! exists and works, could never be called, because its only caller is reached
//! through the key events that were no longer arriving.
//!
//! [`await_with_progress`] keeps the `.await` where it is - the command-palette
//! `Action` trait is `?Send` and takes `&mut Shell`, so the work cannot simply
//! be spawned - and changes only how it is waited for: elapsed time on one
//! line, and Esc or Ctrl-C to give up on it.

use std::future::Future;
use std::io::Write;
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType};
use crossterm::{QueueableCommand, queue};
use futures::StreamExt;

use super::AiService;

/// How often the progress line is redrawn.
const TICK: Duration = Duration::from_millis(100);

/// Wait for `fut`, showing progress and letting the user give up.
///
/// Returns `None` when the user pressed Esc or Ctrl-C; the request is
/// cancelled through [`AiService::cancel_requests`] on the way out.
///
/// **Calling contract**: only from a point where the REPL's own `EventStream`
/// is not being polled - inside a key handler, a command-palette action, or
/// after a `skim` prompt has returned. Every caller today satisfies this
/// because the REPL awaits them; a second reader would race this one for the
/// same keys. It cannot be expressed in the type system, hence the note.
pub async fn await_with_progress<T>(
    label: &str,
    service: &dyn AiService,
    fut: impl Future<Output = T>,
) -> Option<T> {
    // `EventStream::new()` panics with "reader source not set" when stdin is
    // not a terminal, and the progress line would be escape codes in a pipe.
    // Without a terminal there is also nobody to press Esc, so the whole UI
    // collapses to plain `.await`.
    if !interactive() {
        return Some(fut.await);
    }

    let started = Instant::now();
    let mut keys = EventStream::new();
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    tokio::pin!(fut);
    draw(label, started);

    loop {
        tokio::select! {
            value = &mut fut => {
                clear_line();
                return Some(value);
            }
            _ = ticker.tick() => draw(label, started),
            event = keys.next() => match event {
                Some(Ok(event)) if is_cancel(&event) => {
                    // The request is already in flight on the blocking pool;
                    // this is what makes it notice.
                    service.cancel_requests();
                    clear_line();
                    print_line(&format!("{label} cancelled"));
                    return None;
                }
                // Anything else - a keystroke, a resize, a stream error - is
                // ignored rather than buffered. The alternative is answering
                // keys that belong to the prompt this request came from.
                Some(_) => {}
                None => {}
            },
        }
    }
}

/// Is there a person at a terminal to show progress to and take Esc from?
fn interactive() -> bool {
    use std::os::fd::{AsRawFd, BorrowedFd};

    crate::terminal::terminal_control_enabled()
        && nix::unistd::isatty(unsafe { BorrowedFd::borrow_raw(std::io::stdin().as_raw_fd()) })
            .unwrap_or(false)
}

fn is_cancel(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    if key.kind != KeyEventKind::Press {
        return false;
    }
    match key.code {
        KeyCode::Esc => true,
        // Raw mode has ISIG off, so Ctrl-C arrives as a key rather than a
        // signal - the same reason `repl/confirmation.rs` checks for it.
        KeyCode::Char('c') | KeyCode::Char('C') => key.modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

/// Redraw the status line in place.
///
/// `\r` plus a line clear rather than a new line: the caller is in raw mode
/// with the cursor already where it wants it, and a growing column of
/// "Processing... 3s / 4s / 5s" would push their output off the screen.
fn draw(label: &str, started: Instant) {
    let mut out = std::io::stdout();
    queue!(
        out,
        Print("\r"),
        Clear(ClearType::CurrentLine),
        Print(format!(
            "{label} {}s (Esc to cancel)",
            started.elapsed().as_secs()
        ))
    )
    .ok();
    out.flush().ok();
}

fn clear_line() {
    let mut out = std::io::stdout();
    out.queue(Print("\r")).ok();
    out.queue(Clear(ClearType::CurrentLine)).ok();
    out.flush().ok();
}

fn print_line(text: &str) {
    let mut out = std::io::stdout();
    queue!(out, Print(text), Print("\r\n")).ok();
    out.flush().ok();
}
