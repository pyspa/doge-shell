//! Terminal presentation for one `!` turn: the spinner shown while waiting
//! on the provider (`SpinnerGuard`), and incremental Markdown rendering of a
//! streamed answer (`StreamSink`).
use super::*;
use crate::markdown::render_markdown_with_fallback_compact;
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

    /// A spinner that never draws. Tests must not touch the real terminal.
    #[cfg(test)]
    pub(super) fn hidden() -> Self {
        Self {
            progress: ProgressBar::hidden(),
        }
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
/// already does for a complete answer - the same compact renderer, then
/// `ctx.write_stdout` - just spread across many smaller calls instead of one.
pub(super) struct StreamSink<'a> {
    ctx: &'a Context,
    splitter: MarkdownBlockSplitter,
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
        let rendered = render_markdown_with_fallback_compact(block.trim_end());
        if rendered.trim().is_empty() {
            return;
        }
        // `Context::write_stdout` supplies exactly one newline after each
        // block, so consecutive blocks stay on adjacent lines without the
        // prefix newline the normal renderer's blank-line spacing needed.
        let ctx = self.ctx;
        spinner.suspend(|| {
            ctx.write_stdout(&rendered).ok();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::observed_output::{ObservedOutput, SharedOutputObserver};
    use nix::unistd::getpid;
    use std::os::fd::{IntoRawFd, RawFd};

    fn observed_context() -> (Context, SharedOutputObserver, RawFd) {
        let pid = getpid();
        let mut ctx = Context::new_safe(pid, pid, false);
        ctx.interactive = false;
        let outfile = tempfile::tempfile()
            .expect("temporary stdout file")
            .into_raw_fd();
        ctx.outfile = outfile;
        let observer = ObservedOutput::shared(64 * 1024);
        ctx.output_observer = Some(observer.clone());
        (ctx, observer, outfile)
    }

    #[test]
    fn streamed_blocks_are_not_separated_by_a_blank_line() {
        let (ctx, observer, outfile) = observed_context();
        let mut sink = StreamSink::new(&ctx);
        let spinner = SpinnerGuard::hidden();
        sink.write_block(&spinner, "First paragraph.");
        sink.write_block(&spinner, "Second paragraph.");
        drop(spinner);
        nix::unistd::close(outfile).ok();

        let snapshot = observer.lock().expect("observer lock").snapshot();
        assert_eq!(snapshot.stdout, "First paragraph.\nSecond paragraph.\n");
    }

    #[test]
    fn indented_code_blocks_stream_like_the_whole_answer() {
        let (ctx, observer, outfile) = observed_context();
        let mut sink = StreamSink::new(&ctx);
        let spinner = SpinnerGuard::hidden();
        let answer = "Before.\n\n    indented code\n\n    second\n\nAfter.";

        sink.on_delta(&spinner, answer);
        sink.finish_iteration(&spinner);
        drop(spinner);
        nix::unistd::close(outfile).ok();

        let expected = render_markdown_with_fallback_compact(answer.trim_end());
        let snapshot = observer.lock().expect("observer lock").snapshot();
        assert_eq!(
            snapshot.stdout.trim_end_matches('\n'),
            expected.trim_end_matches('\n'),
            "an indented code block must render the same when streamed"
        );
    }
}
