//! Terminal presentation for one `!` turn: the spinner shown while waiting
//! on the provider (`SpinnerGuard`), and incremental Markdown rendering of a
//! streamed answer (`StreamSink`).
use super::*;
use crate::markdown::render_markdown_with_fallback;
use crate::markdown::stream::MarkdownBlockSplitter;
use indicatif::{ProgressBar, ProgressStyle};

pub(super) struct SpinnerGuard {
    progress: ProgressBar,
}

impl SpinnerGuard {
    pub(super) fn start(message: &str) -> Self {
        let progress = ProgressBar::new_spinner();
        // `wide_msg` (rather than `msg`) elides the message to fit the
        // remaining terminal width, so a long in-progress preview
        // (`set_tail`) cannot wrap the spinner onto a second line.
        let style = ProgressStyle::with_template("{spinner} {wide_msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner())
            .tick_chars("-\\|/");
        progress.set_style(style);
        progress.set_message(message.to_string());
        progress.enable_steady_tick(Duration::from_millis(80));
        SpinnerGuard { progress }
    }

    /// Hide the spinner line, run `f`, then let it resume drawing.
    ///
    /// `indicatif` owns the bottom line while ticking; writing to stdout
    /// during that window without this would land the write in the middle
    /// of the spinner's own redraw.
    pub(super) fn suspend<R>(&self, f: impl FnOnce() -> R) -> R {
        self.progress.suspend(f)
    }

    /// Show a single-line preview of text still generating.
    pub(super) fn set_tail(&self, text: &str) {
        let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
        self.progress.set_message(collapsed);
    }
}

impl Drop for SpinnerGuard {
    fn drop(&mut self) {
        self.progress.finish_and_clear();
    }
}

/// Streams one `!` chat turn's answer to the terminal as it arrives, instead
/// of waiting for the whole turn to finish.
///
/// Confirmed Markdown blocks ([`MarkdownBlockSplitter`]) are rendered and
/// written the moment they are safe to render (see that type's docs for why
/// that moment is safe); the unconfirmed remainder is shown as a raw preview
/// on the spinner's status line. This mirrors what `execute_chat_message`
/// already does for a complete answer - `render_markdown_with_fallback`
/// then `ctx.write_stdout` - just spread across many smaller calls instead
/// of one, so the total bytes written for a turn are the same either way.
pub(super) struct StreamSink<'a> {
    ctx: &'a Context,
    splitter: MarkdownBlockSplitter,
    /// Set once anything has been written this turn (any iteration).
    wrote_any: bool,
    /// Reset at the start of each iteration by [`Self::begin_iteration`];
    /// tells the caller whether *this* iteration's response streamed
    /// anything, since a per-round fallback to a non-streaming response can
    /// happen even when the sink itself is active for the whole turn.
    streamed_this_iteration: bool,
}

impl<'a> StreamSink<'a> {
    pub(super) fn new(ctx: &'a Context) -> Self {
        Self {
            ctx,
            splitter: MarkdownBlockSplitter::new(),
            wrote_any: false,
            streamed_this_iteration: false,
        }
    }

    pub(super) fn streamed_this_iteration(&self) -> bool {
        self.streamed_this_iteration
    }

    /// Call once before each `send_chat_streaming` attempt.
    pub(super) fn begin_iteration(&mut self) {
        self.streamed_this_iteration = false;
    }

    /// Feed one text delta, writing any block it completes.
    pub(super) fn on_delta(&mut self, spinner: &SpinnerGuard, text: &str) {
        if !text.is_empty() {
            self.streamed_this_iteration = true;
        }
        for block in self.splitter.push(text) {
            self.write_block(spinner, &block);
        }
        spinner.set_tail(&self.splitter.pending_tail());
    }

    /// Flush whatever is left at the end of one iteration's response, and
    /// reset for the next - a tool-call round and the answer that follows it
    /// are separate Markdown documents, and an open list or fence from one
    /// must not bleed into the other.
    pub(super) fn finish_iteration(&mut self, spinner: &SpinnerGuard) {
        for block in self.splitter.finish() {
            self.write_block(spinner, &block);
        }
        spinner.set_tail("");
    }

    pub(super) fn write_block(&mut self, spinner: &SpinnerGuard, block: &str) {
        let rendered = render_markdown_with_fallback(block.trim());
        if rendered.trim().is_empty() {
            return;
        }
        // `Context::write_stdout` always appends exactly one `\n`, so
        // prefixing every block but the first with one more reproduces the
        // single blank line `TerminalRenderer` puts between any two
        // top-level blocks when rendering the whole answer at once.
        let text = if self.wrote_any {
            format!("\n{rendered}")
        } else {
            rendered
        };
        let ctx = self.ctx;
        spinner.suspend(|| {
            ctx.write_stdout(&text).ok();
        });
        self.wrote_any = true;
    }
}
