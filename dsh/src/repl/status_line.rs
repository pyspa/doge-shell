//! An optional status line pinned to the bottom row of the terminal.
//!
//! # Why a scroll region
//!
//! dsh's prompt is inline, not full-screen: command output scrolls the screen
//! normally. Simply painting the bottom row would leave a copy of the status
//! line burned into the scrollback every time something scrolled, and would
//! break [`crate::repl::render::print_above_prompt`], whose cursor arithmetic
//! assumes nothing but the prompt sits below the cursor.
//!
//! Setting a DECSTBM scroll region (`ESC[1;<rows-1>r`) instead puts the last
//! row physically outside the scrolling area. Every existing drawing routine
//! then keeps working unchanged.
//!
//! # Why it is off by default
//!
//! DECSTBM support varies, and a terminal left with a stale margin looks
//! broken. Enable with `(pref-status-line t)`; `DSH_STATUS_LINE=0` forces it
//! off regardless.

use crate::input::display_width;
use crossterm::{
    cursor,
    style::Print,
    terminal::{Clear, ClearType},
};
use std::io::Write;

/// Below this many rows, reserving one for status leaves too little to work in.
const MIN_ROWS: u16 = 3;

#[derive(Debug, Default)]
pub(crate) struct StatusLine {
    enabled: bool,
    /// Whether the scroll region is currently reserved.
    armed: bool,
    rows: u16,
    columns: u16,
    /// Last content drawn, so an unchanged status costs no output.
    last: String,
}

impl StatusLine {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled: enabled && env_allows(),
            ..Default::default()
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled && env_allows();
    }

    pub fn set_size(&mut self, columns: u16, rows: u16) {
        self.columns = columns;
        self.rows = rows;
    }

    fn usable(&self) -> bool {
        self.enabled && self.rows >= MIN_ROWS && self.columns > 0
    }

    /// Reserves the bottom row. Safe to call repeatedly.
    pub fn arm<W: Write>(&mut self, out: &mut W) {
        if self.armed || !self.usable() {
            return;
        }
        // Setting the margin homes the cursor, so bracket it with save/restore.
        write!(out, "\x1b7\x1b[1;{}r\x1b8", self.rows - 1).ok();
        self.armed = true;
        self.last.clear();
    }

    /// Releases the scroll region and clears the row.
    ///
    /// Must run before anything that takes over the screen — a full-screen UI,
    /// a foreground child process, or shell exit — because a margin left behind
    /// makes the terminal look broken.
    pub fn disarm<W: Write>(&mut self, out: &mut W) {
        if !self.armed {
            return;
        }
        write!(out, "\x1b7\x1b[r").ok();
        if self.rows > 0 {
            crossterm::queue!(
                out,
                cursor::MoveTo(0, self.rows - 1),
                Clear(ClearType::CurrentLine)
            )
            .ok();
        }
        write!(out, "\x1b8").ok();
        self.armed = false;
        self.last.clear();
    }

    /// Forgets what was last drawn, so the next [`Self::render`] repaints even
    /// if the content is unchanged.
    ///
    /// Needed because DECSTBM protects the reserved row from *scrolling*, not
    /// from erasure: an `ED` (`Clear(ClearType::FromCursorDown)`) wipes it
    /// regardless. Anything that erases below the cursor must call this, or the
    /// dedup check keeps the row blank until the content happens to change —
    /// on an idle shell, possibly never.
    pub fn invalidate(&mut self) {
        self.last.clear();
    }

    /// Draws `content` on the reserved row, skipping the write when it has not
    /// changed.
    pub fn render<W: Write>(&mut self, out: &mut W, content: &str) {
        if !self.usable() {
            return;
        }
        self.arm(out);
        if content == self.last {
            return;
        }

        let text = truncate_to_width(content, self.columns as usize);
        write!(out, "\x1b7").ok();
        crossterm::queue!(
            out,
            cursor::MoveTo(0, self.rows - 1),
            Clear(ClearType::CurrentLine),
            Print(&text)
        )
        .ok();
        write!(out, "\x1b8").ok();
        self.last = content.to_string();
    }
}

/// `DSH_STATUS_LINE=0` (or `false`/`no`/`off`) hard-disables the feature.
fn env_allows() -> bool {
    match std::env::var("DSH_STATUS_LINE") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// Trims to `columns` display cells, counting wide characters as two and
/// ignoring ANSI sequences.
fn truncate_to_width(content: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    if display_width(content) <= columns {
        return content.to_string();
    }

    let mut out = String::new();
    let mut width = 0;
    for ch in content.chars() {
        let ch_width = display_width(&ch.to_string());
        if width + ch_width > columns {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out
}

/// Builds the status text from state the shell already keeps up to date.
///
/// Everything read here is a cached value maintained by an existing background
/// task, so composing never does I/O — a status line must not make the prompt
/// slower.
pub(crate) fn compose(
    scheduler: &crate::scheduler::SchedulerState,
    job_count: usize,
    git: Option<&crate::prompt::GitStatus>,
    github: Option<&crate::github::GitHubStatus>,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    let tasks = scheduler.views();
    if !tasks.is_empty() {
        let failing = tasks
            .iter()
            .filter(|task| task.last.as_ref().is_some_and(|run| !run.succeeded()))
            .count();
        let running = tasks.iter().filter(|task| task.running).count();

        let mut summary = format!("⏱ {}", tasks.len());
        if running > 0 {
            summary.push_str(&format!(" running {running}"));
        }
        if failing > 0 {
            summary.push_str(&format!(" failing {failing}"));
        }
        if !scheduler.enabled {
            summary.push_str(" paused");
        }
        parts.push(summary);
    }

    if job_count > 0 {
        parts.push(format!("⚙ {job_count} job{}", plural(job_count)));
    }

    if let Some(git) = git {
        let mut branch = format!(" {}", git.branch);
        let dirty = git.modified + git.untracked + git.staged + git.conflicted;
        if dirty > 0 {
            branch.push_str(&format!(" ●{dirty}"));
        }
        if git.ahead > 0 {
            branch.push_str(&format!(" ↑{}", git.ahead));
        }
        if git.behind > 0 {
            branch.push_str(&format!(" ↓{}", git.behind));
        }
        parts.push(branch);
    }

    if let Some(github) = github
        && github.total() > 0
    {
        parts.push(format!("🐙 {}", github.total()));
    }

    parts.join("  ")
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Shared handle to the status line.
///
/// `Rc<RefCell<..>>` rather than a plain field so [`StatusLinePause`] can be
/// created without holding a borrow of the `Repl` — the call sites that need
/// to pause it also need `&mut Repl` for their real work. The REPL is
/// single-threaded (`Repl` holds `&mut Shell`, which is `!Send`), so there is
/// no cross-thread access to guard against.
pub(crate) type SharedStatusLine = std::rc::Rc<std::cell::RefCell<StatusLine>>;

pub(crate) fn shared(enabled: bool) -> SharedStatusLine {
    std::rc::Rc::new(std::cell::RefCell::new(StatusLine::new(enabled)))
}

/// Releases the scroll region for as long as it is alive, restoring it on
/// every return path.
///
/// Wrap anything that takes over the screen: the history picker, the block
/// browser, skim, the completion grid, the external editor, and foreground
/// command execution. Without this, a full-screen UI would draw inside a
/// shortened scroll region, and a child process would inherit a margin it
/// knows nothing about.
///
/// The writer is a parameter rather than a hard-coded `std::io::stdout()` so
/// tests can drive the guard without leaving a DECSTBM margin on the terminal
/// that is running `cargo test`.
pub(crate) struct StatusLinePause<W: Write = std::io::Stdout> {
    status: SharedStatusLine,
    out: W,
    /// Whether it was armed before, and so should be re-armed after.
    rearm: bool,
}

impl StatusLinePause<std::io::Stdout> {
    pub fn new(status: SharedStatusLine) -> Self {
        Self::with_writer(status, std::io::stdout())
    }
}

impl<W: Write> StatusLinePause<W> {
    pub fn with_writer(status: SharedStatusLine, mut out: W) -> Self {
        let rearm = status.borrow().armed;
        if rearm {
            status.borrow_mut().disarm(&mut out);
            out.flush().ok();
        }
        Self { status, out, rearm }
    }
}

impl<W: Write> Drop for StatusLinePause<W> {
    fn drop(&mut self) {
        if self.rearm {
            self.status.borrow_mut().arm(&mut self.out);
            self.out.flush().ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::SchedulerState;
    use dsh_types::schedule::{NotifyPolicy, SchedTaskSpec, parse_interval};
    use std::collections::HashMap;
    use std::time::Duration;

    fn status(rows: u16, columns: u16) -> StatusLine {
        let mut status = StatusLine::new(true);
        status.set_size(columns, rows);
        status
    }

    fn rendered(status: &mut StatusLine, content: &str) -> String {
        let mut out: Vec<u8> = Vec::new();
        status.render(&mut out, content);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn disabled_status_writes_nothing() {
        let mut status = StatusLine::new(false);
        status.set_size(80, 24);
        assert!(rendered(&mut status, "hello").is_empty());
    }

    #[test]
    fn arming_sets_a_scroll_region_one_row_short() {
        let mut status = status(24, 80);
        let output = rendered(&mut status, "hello");
        assert!(output.contains("\x1b[1;23r"), "missing DECSTBM: {output:?}");
        assert!(output.contains("hello"));
    }

    #[test]
    fn disarming_releases_the_region() {
        let mut status = status(24, 80);
        rendered(&mut status, "hello");

        let mut out: Vec<u8> = Vec::new();
        status.disarm(&mut out);
        let output = String::from_utf8(out).unwrap();
        assert!(output.contains("\x1b[r"), "margin not reset: {output:?}");
    }

    /// Redrawing identical content on every 1-second tick would flicker.
    #[test]
    fn unchanged_content_is_not_redrawn() {
        let mut status = status(24, 80);
        rendered(&mut status, "hello");
        assert!(rendered(&mut status, "hello").is_empty());
        assert!(!rendered(&mut status, "other").is_empty());
    }

    /// An `ED` erase wipes the reserved row even though DECSTBM is set, so
    /// callers invalidate; without this the dedup check would keep the row
    /// blank until the content happened to change.
    #[test]
    fn invalidating_forces_a_repaint_of_identical_content() {
        let mut status = status(24, 80);
        rendered(&mut status, "hello");
        assert!(rendered(&mut status, "hello").is_empty());

        status.invalidate();
        let repaint = rendered(&mut status, "hello");
        assert!(repaint.contains("hello"), "not repainted: {repaint:?}");
    }

    #[test]
    fn a_tiny_terminal_is_left_alone() {
        let mut too_short = status(2, 80);
        assert!(rendered(&mut too_short, "hello").is_empty());

        // Width 0 means we never learned the terminal size (not a tty).
        let mut unsized_terminal = status(24, 0);
        assert!(rendered(&mut unsized_terminal, "hello").is_empty());
    }

    #[test]
    fn content_is_truncated_to_the_terminal_width() {
        assert_eq!(truncate_to_width("hello world", 5), "hello");
        assert_eq!(truncate_to_width("hello", 80), "hello");
        assert_eq!(truncate_to_width("hello", 0), "");
        // Wide characters count as two cells and are never split.
        assert_eq!(truncate_to_width("日本語", 4), "日本");
        assert_eq!(truncate_to_width("日本語", 5), "日本");
    }

    #[test]
    fn pausing_and_resuming_restores_the_region() {
        let shared = shared(true);
        shared.borrow_mut().set_size(80, 24);
        let mut sink: Vec<u8> = Vec::new();
        shared.borrow_mut().render(&mut sink, "hello");
        assert!(shared.borrow().armed);

        {
            // Never the real stdout: the guard writes DECSTBM, and a margin
            // left on the developer's terminal survives the test run.
            let _pause = StatusLinePause::with_writer(shared.clone(), Vec::new());
            assert!(!shared.borrow().armed, "region held during the pause");
        }
        // The guard re-arms on drop, so later draws still land off-screen.
        assert!(shared.borrow().armed);
    }

    #[test]
    fn pausing_an_unarmed_status_is_a_no_op() {
        let shared = shared(false);
        {
            let _pause = StatusLinePause::with_writer(shared.clone(), Vec::new());
        }
        assert!(!shared.borrow().armed);
    }

    // --- compose ---

    fn scheduler_with(command: &str, name: &str) -> SchedulerState {
        let mut state = SchedulerState::new();
        state
            .add(
                SchedTaskSpec {
                    name: name.to_string(),
                    interval: parse_interval("5m").unwrap(),
                    command: command.to_string(),
                    cwd: "/tmp".to_string(),
                    notify: NotifyPolicy::Both,
                    timeout: Duration::from_secs(10),
                },
                HashMap::new(),
            )
            .unwrap();
        state
    }

    #[test]
    fn an_idle_shell_has_an_empty_status() {
        assert_eq!(compose(&SchedulerState::new(), 0, None, None), "");
    }

    #[test]
    fn tasks_and_jobs_are_summarised() {
        let state = scheduler_with("true", "fetch");
        let line = compose(&state, 2, None, None);
        assert!(line.contains("⏱ 1"), "{line}");
        assert!(line.contains("2 jobs"), "{line}");
    }

    #[test]
    fn one_job_is_singular() {
        let line = compose(&SchedulerState::new(), 1, None, None);
        assert!(line.contains("1 job"));
        assert!(!line.contains("jobs"));
    }

    #[test]
    fn failing_tasks_are_called_out() {
        let mut state = scheduler_with("false", "fetch");
        state.record(1, "", 1, false, Duration::from_millis(1));
        assert!(compose(&state, 0, None, None).contains("failing 1"));
    }

    #[test]
    fn a_paused_scheduler_says_so() {
        let mut state = scheduler_with("true", "fetch");
        state.enabled = false;
        assert!(compose(&state, 0, None, None).contains("paused"));
    }

    #[test]
    fn git_state_is_rendered_compactly() {
        let git = crate::prompt::GitStatus {
            branch: "main".to_string(),
            modified: 2,
            ahead: 1,
            ..Default::default()
        };
        let line = compose(&SchedulerState::new(), 0, Some(&git), None);
        assert!(line.contains("main"), "{line}");
        assert!(line.contains("●2"), "{line}");
        assert!(line.contains("↑1"), "{line}");
    }

    #[test]
    fn github_appears_only_when_there_is_something_to_show() {
        let empty = crate::github::GitHubStatus::default();
        assert!(!compose(&SchedulerState::new(), 0, None, Some(&empty)).contains("🐙"));

        let pending = crate::github::GitHubStatus {
            review_count: 3,
            ..Default::default()
        };
        assert!(compose(&SchedulerState::new(), 0, None, Some(&pending)).contains("🐙 3"));
    }
}
