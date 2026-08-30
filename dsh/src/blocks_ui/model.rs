//! Pure state for the block browser.
//!
//! No terminal access lives here, so every interaction is unit-testable.
//! Side effects the driver must perform (clipboard writes) come back as
//! [`BrowserAction`] values rather than being done inline.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dsh_types::ansi;
use dsh_types::command_block::CommandBlock;
use std::collections::HashSet;

/// Lines shown for a folded block before the "N more" marker.
const FOLDED_LINES: usize = 5;

/// Blocks whose output is this proportion of escape sequences are full-screen
/// programs (`vim`, `htop`); their stripped output is noise, so they start
/// folded.
const NOISY_ANSI_DENSITY: f32 = 0.5;

/// Which captured stream the output pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Both,
    Stdout,
    Stderr,
}

impl OutputStream {
    fn next(self) -> Self {
        match self {
            OutputStream::Both => OutputStream::Stdout,
            OutputStream::Stdout => OutputStream::Stderr,
            OutputStream::Stderr => OutputStream::Both,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            OutputStream::Both => "both",
            OutputStream::Stdout => "stdout",
            OutputStream::Stderr => "stderr",
        }
    }
}

/// Which pane the movement keys act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Output,
}

/// What the browser wants to hand back to the REPL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserOutcome {
    /// Put the text in the input buffer and leave it there.
    Insert(String),
    /// Put the text in the input buffer and execute it.
    Run(String),
    /// Leave the input buffer alone.
    Quit,
}

/// What the driver should do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserAction {
    Redraw,
    Noop,
    /// Copy this to the clipboard; the driver reports success in the status line.
    Copy(String),
    Finish(BrowserOutcome),
}

pub struct BlockBrowser {
    /// Newest first, matching the order the list is drawn in.
    blocks: Vec<CommandBlock>,
    /// Indices into `blocks` that pass the active filters.
    filtered: Vec<usize>,
    selected: usize,
    focus: Focus,

    filter: String,
    /// True while `/` is capturing the filter text.
    filter_input: bool,
    failed_only: bool,
    watched_only: bool,

    stream: OutputStream,
    wrap: bool,
    /// Block indices whose output is collapsed.
    folded: HashSet<usize>,
    /// Block indices marked for `x` (runbook export).
    marked: HashSet<usize>,
    output_scroll: usize,
    /// Rendered output for one block, keyed by the inputs that produced it, so
    /// a 1 MiB block is not re-split on every frame.
    output_cache: Option<(usize, OutputStream, Vec<String>)>,

    show_help: bool,
    status: Option<String>,
    /// Rows the output pane can display; drives paging.
    output_height: usize,
}

impl BlockBrowser {
    /// `blocks` must be in `CommandBlockHistory::get_all_blocks` order, which is
    /// newest first (`push` uses `push_front`).
    ///
    /// The order is load-bearing beyond display: `blocks explain N` numbers the
    /// same sequence from 1, so reordering here would silently explain the wrong
    /// block.
    pub fn new(blocks: Vec<CommandBlock>) -> Self {
        // Full-screen program output is unreadable once stripped; do not make
        // the user fold it by hand.
        let folded = blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| {
                ansi::ansi_density(&block.stdout) > NOISY_ANSI_DENSITY
                    || ansi::ansi_density(&block.stderr) > NOISY_ANSI_DENSITY
            })
            .map(|(index, _)| index)
            .collect();

        let mut browser = Self {
            blocks,
            filtered: Vec::new(),
            selected: 0,
            focus: Focus::List,
            filter: String::new(),
            filter_input: false,
            failed_only: false,
            watched_only: false,
            stream: OutputStream::Both,
            wrap: false,
            folded,
            marked: HashSet::new(),
            output_scroll: 0,
            output_cache: None,
            show_help: false,
            status: None,
            output_height: 10,
        };
        browser.recompute();
        browser
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn matched(&self) -> usize {
        self.filtered.len()
    }

    pub fn total(&self) -> usize {
        self.blocks.len()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn filter_input(&self) -> bool {
        self.filter_input
    }

    pub fn wrap(&self) -> bool {
        self.wrap
    }

    pub fn stream(&self) -> OutputStream {
        self.stream
    }

    pub fn show_help(&self) -> bool {
        self.show_help
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status = Some(message.into());
    }

    pub fn output_scroll(&self) -> usize {
        self.output_scroll
    }

    pub fn set_output_height(&mut self, height: usize) {
        self.output_height = height.max(1);
    }

    pub fn selected_block(&self) -> Option<&CommandBlock> {
        self.filtered.get(self.selected).map(|i| &self.blocks[*i])
    }

    pub fn blocks(&self) -> Vec<&CommandBlock> {
        self.filtered.iter().map(|i| &self.blocks[*i]).collect()
    }

    fn recompute(&mut self) {
        let needle = self.filter.to_lowercase();
        self.filtered = self
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| {
                if self.failed_only && block.exit_code == 0 {
                    return false;
                }
                if self.watched_only && !block.watched {
                    return false;
                }
                if needle.is_empty() {
                    return true;
                }
                // Search the command and its output, which is the whole point
                // of keeping the output around.
                block.command.to_lowercase().contains(&needle)
                    || block.stdout.to_lowercase().contains(&needle)
                    || block.stderr.to_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect();

        self.selected = match self.filtered.len() {
            0 => 0,
            len => self.selected.min(len - 1),
        };
        self.reset_output_view();
    }

    fn reset_output_view(&mut self) {
        self.output_scroll = 0;
        self.output_cache = None;
    }

    /// Lines of the selected block's output, folded if requested.
    ///
    /// Returns the visible lines plus the number hidden by folding.
    pub fn output_lines(&mut self) -> (Vec<String>, usize) {
        let Some(&block_index) = self.filtered.get(self.selected) else {
            return (Vec::new(), 0);
        };

        let cached = matches!(
            &self.output_cache,
            Some((index, stream, _)) if *index == block_index && *stream == self.stream
        );
        if !cached {
            let block = &self.blocks[block_index];
            let text = match self.stream {
                OutputStream::Stdout => block.stdout.clone(),
                OutputStream::Stderr => block.stderr.clone(),
                OutputStream::Both => {
                    if block.stderr.is_empty() {
                        block.stdout.clone()
                    } else if block.stdout.is_empty() {
                        block.stderr.clone()
                    } else {
                        format!("{}\n{}", block.stdout, block.stderr)
                    }
                }
            };
            let lines = ansi::display_lines(&text);
            self.output_cache = Some((block_index, self.stream, lines));
        }

        let lines = match &self.output_cache {
            Some((_, _, lines)) => lines,
            None => return (Vec::new(), 0),
        };

        if self.folded.contains(&block_index) && lines.len() > FOLDED_LINES {
            let hidden = lines.len() - FOLDED_LINES;
            return (lines[..FOLDED_LINES].to_vec(), hidden);
        }
        (lines.clone(), 0)
    }

    pub fn is_folded(&self) -> bool {
        self.filtered
            .get(self.selected)
            .is_some_and(|index| self.folded.contains(index))
    }

    /// Why the selected block has no output, when it has none.
    ///
    /// Output is only observed for a foreground external command that is not
    /// redirected, not part of a pipeline and not PTY-proxied
    /// (`process::job_process::observe_foreground_external`), so an empty block
    /// is normal rather than a bug — say so instead of showing a blank pane.
    pub fn empty_output_note(&self) -> Option<&'static str> {
        let block = self.selected_block()?;
        if !block.stdout.is_empty() || !block.stderr.is_empty() {
            return None;
        }
        Some(
            "(no output captured — builtins, redirected commands and pipeline stages are not observed)",
        )
    }

    /// Whether the selected block's output hit the capture cap.
    ///
    /// `append_bounded` keeps the *tail*, so a truncated block shows the end of
    /// the run; without saying so the user misreads their own logs.
    pub fn is_truncated(&self) -> bool {
        self.selected_block().is_some_and(|block| {
            block.stdout.starts_with("... (truncated)")
                || block.stderr.starts_with("... (truncated)")
                || block.stdout.ends_with("... (truncated)")
                || block.stderr.ends_with("... (truncated)")
        })
    }

    fn move_selection(&mut self, delta: isize) -> BrowserAction {
        if self.filtered.is_empty() {
            return BrowserAction::Noop;
        }
        let last = self.filtered.len() - 1;
        let next = if delta < 0 {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected.saturating_add(delta as usize).min(last)
        };
        if next == self.selected {
            return BrowserAction::Noop;
        }
        self.selected = next;
        self.reset_output_view();
        BrowserAction::Redraw
    }

    fn scroll_output(&mut self, delta: isize) -> BrowserAction {
        let (lines, _) = self.output_lines();
        // Keep at least one row on screen when scrolled to the bottom.
        let max_scroll = lines.len().saturating_sub(1);
        let next = if delta < 0 {
            self.output_scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.output_scroll
                .saturating_add(delta as usize)
                .min(max_scroll)
        };
        if next == self.output_scroll {
            return BrowserAction::Noop;
        }
        self.output_scroll = next;
        BrowserAction::Redraw
    }

    /// Clamp the scroll offset into range, e.g. after the terminal shrinks.
    pub fn clamp_scroll(&mut self) {
        let (lines, _) = self.output_lines();
        let max_scroll = lines.len().saturating_sub(1);
        self.output_scroll = self.output_scroll.min(max_scroll);
    }

    fn toggle_fold(&mut self) -> BrowserAction {
        let Some(&index) = self.filtered.get(self.selected) else {
            return BrowserAction::Noop;
        };
        if !self.folded.remove(&index) {
            self.folded.insert(index);
        }
        self.output_scroll = 0;
        BrowserAction::Redraw
    }

    pub fn on_key(&mut self, key: KeyEvent) -> BrowserAction {
        const CTRL: KeyModifiers = KeyModifiers::CONTROL;
        // Any key dismisses a stale "copied" message.
        self.status = None;

        if self.show_help {
            self.show_help = false;
            return BrowserAction::Redraw;
        }

        // While `/` is active every printable key edits the filter.
        if self.filter_input {
            return match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => {
                    self.filter_input = false;
                    self.filter.clear();
                    self.recompute();
                    BrowserAction::Redraw
                }
                (KeyCode::Enter, _) => {
                    self.filter_input = false;
                    BrowserAction::Redraw
                }
                (KeyCode::Backspace, _) => {
                    if self.filter.pop().is_none() {
                        return BrowserAction::Noop;
                    }
                    self.recompute();
                    BrowserAction::Redraw
                }
                (KeyCode::Char(ch), m) if !m.contains(CTRL) => {
                    self.filter.push(ch);
                    self.recompute();
                    BrowserAction::Redraw
                }
                _ => BrowserAction::Noop,
            };
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) | (KeyCode::Char('c'), CTRL) => {
                BrowserAction::Finish(BrowserOutcome::Quit)
            }
            (KeyCode::Char('?'), _) => {
                self.show_help = true;
                BrowserAction::Redraw
            }

            (KeyCode::Tab, _) => {
                self.focus = match self.focus {
                    Focus::List => Focus::Output,
                    Focus::Output => Focus::List,
                };
                BrowserAction::Redraw
            }

            // Movement acts on whichever pane has focus.
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => match self.focus {
                Focus::List => self.move_selection(1),
                Focus::Output => self.scroll_output(1),
            },
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => match self.focus {
                Focus::List => self.move_selection(-1),
                Focus::Output => self.scroll_output(-1),
            },
            (KeyCode::Char('g'), _) => match self.focus {
                Focus::List => self.move_selection(isize::MIN / 2),
                Focus::Output => self.scroll_output(isize::MIN / 2),
            },
            (KeyCode::Char('G'), _) => match self.focus {
                Focus::List => self.move_selection(isize::MAX / 2),
                Focus::Output => self.scroll_output(isize::MAX / 2),
            },

            (KeyCode::Char('d'), CTRL) | (KeyCode::PageDown, _) => {
                self.scroll_output(self.output_height as isize)
            }
            (KeyCode::Char('u'), CTRL) | (KeyCode::PageUp, _) => {
                self.scroll_output(-(self.output_height as isize))
            }

            (KeyCode::Char(' '), _) => self.toggle_fold(),
            (KeyCode::Char('W'), _) => {
                self.wrap = !self.wrap;
                BrowserAction::Redraw
            }
            (KeyCode::Char('s'), _) => {
                self.stream = self.stream.next();
                self.reset_output_view();
                BrowserAction::Redraw
            }

            (KeyCode::Char('/'), _) => {
                self.filter_input = true;
                BrowserAction::Redraw
            }
            (KeyCode::Char('f'), _) => {
                self.failed_only = !self.failed_only;
                self.recompute();
                BrowserAction::Redraw
            }
            (KeyCode::Char('w'), _) => {
                self.watched_only = !self.watched_only;
                self.recompute();
                BrowserAction::Redraw
            }

            (KeyCode::Char('c'), _) => match self.selected_block() {
                Some(block) => BrowserAction::Copy(block.command.clone()),
                None => BrowserAction::Noop,
            },
            (KeyCode::Char('y'), _) => {
                let (lines, _) = self.output_lines();
                if lines.is_empty() {
                    return BrowserAction::Noop;
                }
                BrowserAction::Copy(lines.join("\n"))
            }

            (KeyCode::Enter, _) => match self.selected_block() {
                Some(block) => BrowserAction::Finish(BrowserOutcome::Insert(block.command.clone())),
                None => BrowserAction::Noop,
            },
            (KeyCode::Char('r'), _) => match self.selected_block() {
                Some(block) => BrowserAction::Finish(BrowserOutcome::Run(block.command.clone())),
                None => BrowserAction::Noop,
            },
            (KeyCode::Char('d'), _) => match self.selected_block().and_then(|b| b.cwd.clone()) {
                Some(cwd) => {
                    BrowserAction::Finish(BrowserOutcome::Run(format!("cd {}", quote_path(&cwd))))
                }
                None => BrowserAction::Noop,
            },
            (KeyCode::Char('e'), _) => match self.explain_command() {
                // Routed back through the shell: an AI call cannot run inside
                // the synchronous RunInteractive closure.
                Some(command) => BrowserAction::Finish(BrowserOutcome::Run(command)),
                None => BrowserAction::Noop,
            },

            (KeyCode::Char('m'), _) => self.toggle_mark(),
            (KeyCode::Char('x'), _) => match self.export_command() {
                // Routed back through the shell like `e`: file writing and the
                // optional AI pass live in the `blocks` builtin.
                Some(command) => BrowserAction::Finish(BrowserOutcome::Run(command)),
                None => BrowserAction::Noop,
            },

            _ => BrowserAction::Noop,
        }
    }

    fn toggle_mark(&mut self) -> BrowserAction {
        let Some(&index) = self.filtered.get(self.selected) else {
            return BrowserAction::Noop;
        };
        if !self.marked.remove(&index) {
            self.marked.insert(index);
        }
        self.status = Some(format!("{} marked for export", self.marked.len()));
        BrowserAction::Redraw
    }

    /// Whether the block at this position in the filtered list is marked.
    pub fn is_marked(&self, filtered_pos: usize) -> bool {
        self.filtered
            .get(filtered_pos)
            .is_some_and(|index| self.marked.contains(index))
    }

    pub fn marked_count(&self) -> usize {
        self.marked.len()
    }

    /// `blocks export --ids … -o runbook-<timestamp>.md` for the marked
    /// blocks, or the selected one when nothing is marked.
    ///
    /// Ids rather than display indices: this command runs after the browser
    /// closes, and the export itself shifts every display index by one.
    fn export_command(&self) -> Option<String> {
        let mut ids: Vec<u64> = if self.marked.is_empty() {
            vec![self.selected_block()?.id]
        } else {
            self.marked
                .iter()
                .filter_map(|&index| self.blocks.get(index))
                .map(|block| block.id)
                .collect()
        };
        ids.sort_unstable();
        let ids = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let file = format!(
            "runbook-{}.md",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        Some(format!("blocks export --ids {ids} -o {file}"))
    }

    /// `blocks explain N`, where N is the 1-based index `blocks list` shows.
    ///
    /// Numbered against the unfiltered list: the builtin indexes
    /// `get_all_blocks()`, so the position within the current filter would point
    /// at a different block whenever a filter is active.
    fn explain_command(&self) -> Option<String> {
        let block_index = *self.filtered.get(self.selected)?;
        Some(format!("blocks explain {}", block_index + 1))
    }
}

/// Quote a path for the shell when it contains anything that would be split or
/// expanded.
pub fn quote_path(path: &str) -> String {
    let needs_quotes = path.is_empty()
        || path
            .chars()
            .any(|ch| ch.is_whitespace() || "\"'\\$`*?[]{}()<>|&;#~!".contains(ch));
    if !needs_quotes {
        return path.to_string();
    }
    format!("'{}'", path.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn block(id: u64, command: &str, exit_code: i32, stdout: &str) -> CommandBlock {
        CommandBlock {
            id,
            command: command.to_string(),
            cwd: Some("/repo".to_string()),
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code,
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            duration_ms: 100,
            output_entry_ids: Vec::new(),
            watched: false,
            watch_summary: None,
        }
    }

    /// `get_all_blocks` order: newest first, because `push` uses `push_front`.
    fn sample() -> BlockBrowser {
        BlockBrowser::new(vec![
            block(3, "git status", 0, "clean"),
            block(2, "cargo test", 1, "test failed"),
            block(1, "cargo build", 0, "compiling\ndone"),
        ])
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    fn commands(browser: &BlockBrowser) -> Vec<String> {
        browser
            .blocks()
            .into_iter()
            .map(|b| b.command.clone())
            .collect()
    }

    #[test]
    fn mark_toggles_and_reports_count() {
        let mut b = sample();
        assert_eq!(b.marked_count(), 0);
        assert_eq!(b.on_key(key(KeyCode::Char('m'))), BrowserAction::Redraw);
        assert!(b.is_marked(0));
        assert_eq!(b.marked_count(), 1);
        assert_eq!(b.status(), Some("1 marked for export"));

        b.on_key(key(KeyCode::Char('m')));
        assert!(!b.is_marked(0));
        assert_eq!(b.marked_count(), 0);
    }

    #[test]
    fn export_uses_marked_ids_sorted_or_falls_back_to_selection() {
        let mut b = sample();
        // Mark "git status" (id 3) and "cargo build" (id 1).
        b.on_key(key(KeyCode::Char('m')));
        b.on_key(key(KeyCode::Char('G')));
        b.on_key(key(KeyCode::Char('m')));

        let action = b.on_key(key(KeyCode::Char('x')));
        let BrowserAction::Finish(BrowserOutcome::Run(command)) = action else {
            panic!("expected export command, got {action:?}");
        };
        assert!(command.starts_with("blocks export --ids 1,3 -o runbook-"));
        assert!(command.ends_with(".md"));

        // No marks: export the selected block by its stable id.
        let mut b = sample();
        b.on_key(key(KeyCode::Char('j')));
        let action = b.on_key(key(KeyCode::Char('x')));
        let BrowserAction::Finish(BrowserOutcome::Run(command)) = action else {
            panic!("expected export command, got {action:?}");
        };
        assert!(command.starts_with("blocks export --ids 2 -o runbook-"));
    }

    #[test]
    fn export_ids_survive_an_active_filter() {
        let mut b = sample();
        b.on_key(key(KeyCode::Char('/')));
        for ch in "cargo build".chars() {
            b.on_key(key(KeyCode::Char(ch)));
        }
        b.on_key(key(KeyCode::Enter));
        assert_eq!(commands(&b), vec!["cargo build"]);

        b.on_key(key(KeyCode::Char('m')));
        let action = b.on_key(key(KeyCode::Char('x')));
        let BrowserAction::Finish(BrowserOutcome::Run(command)) = action else {
            panic!("expected export command, got {action:?}");
        };
        // "cargo build" has stable id 1, not its filtered position.
        assert!(command.starts_with("blocks export --ids 1 -o runbook-"));
    }

    #[test]
    fn blocks_keep_the_history_order_which_is_newest_first() {
        // Reordering here would desync the `blocks explain N` numbering.
        assert_eq!(
            commands(&sample()),
            vec!["git status", "cargo test", "cargo build"]
        );
    }

    #[test]
    fn selection_moves_and_stops_at_the_ends() {
        let mut b = sample();
        assert_eq!(b.selected(), 0);
        assert_eq!(b.on_key(key(KeyCode::Up)), BrowserAction::Noop);

        assert_eq!(b.on_key(key(KeyCode::Char('j'))), BrowserAction::Redraw);
        assert_eq!(b.selected(), 1);
        b.on_key(key(KeyCode::Char('G')));
        assert_eq!(b.selected(), 2);
        assert_eq!(b.on_key(key(KeyCode::Char('j'))), BrowserAction::Noop);
        b.on_key(key(KeyCode::Char('g')));
        assert_eq!(b.selected(), 0);
    }

    #[test]
    fn filter_matches_command_and_output_case_insensitively() {
        let mut b = sample();
        b.on_key(key(KeyCode::Char('/')));
        assert!(b.filter_input());
        for ch in "CARGO".chars() {
            b.on_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(commands(&b), vec!["cargo test", "cargo build"]);

        // "compiling" only appears in the output of `cargo build`.
        for _ in 0..5 {
            b.on_key(key(KeyCode::Backspace));
        }
        for ch in "compiling".chars() {
            b.on_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(commands(&b), vec!["cargo build"]);
    }

    #[test]
    fn esc_during_filter_input_clears_it_instead_of_quitting() {
        let mut b = sample();
        b.on_key(key(KeyCode::Char('/')));
        for ch in "status".chars() {
            b.on_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(b.matched(), 1);

        assert_eq!(b.on_key(key(KeyCode::Esc)), BrowserAction::Redraw);
        assert!(!b.filter_input());
        assert_eq!(b.filter(), "");
        assert_eq!(b.matched(), 3);
    }

    #[test]
    fn filter_input_captures_keys_that_are_commands_outside_it() {
        let mut b = sample();
        b.on_key(key(KeyCode::Char('/')));
        // 'q' would quit outside filter mode.
        assert_eq!(b.on_key(key(KeyCode::Char('q'))), BrowserAction::Redraw);
        assert_eq!(b.filter(), "q");
    }

    #[test]
    fn failed_filter_excludes_zero_exit_blocks() {
        let mut b = sample();
        assert_eq!(b.on_key(key(KeyCode::Char('f'))), BrowserAction::Redraw);
        assert_eq!(commands(&b), vec!["cargo test"]);
        b.on_key(key(KeyCode::Char('f')));
        assert_eq!(b.matched(), 3);
    }

    #[test]
    fn watched_filter_keeps_only_ai_watched_blocks() {
        let mut watched = block(4, "ai-watch -- make", 0, "out");
        watched.watched = true;
        let mut b = BlockBrowser::new(vec![block(1, "ls", 0, ""), watched]);

        b.on_key(key(KeyCode::Char('w')));
        assert_eq!(commands(&b), vec!["ai-watch -- make"]);
    }

    #[test]
    fn selection_clamps_when_the_filter_shrinks_the_list() {
        let mut b = sample();
        b.on_key(key(KeyCode::Char('G')));
        assert_eq!(b.selected(), 2);

        b.on_key(key(KeyCode::Char('f'))); // one failed block
        assert_eq!(b.matched(), 1);
        assert_eq!(b.selected(), 0);
        assert_eq!(b.selected_block().unwrap().command, "cargo test");
    }

    #[test]
    fn tab_switches_focus_so_movement_scrolls_the_output() {
        let mut b = BlockBrowser::new(vec![block(1, "seq", 0, "1\n2\n3\n4\n5\n6")]);
        assert_eq!(b.focus(), Focus::List);

        b.on_key(key(KeyCode::Tab));
        assert_eq!(b.focus(), Focus::Output);
        assert_eq!(b.on_key(key(KeyCode::Char('j'))), BrowserAction::Redraw);
        assert_eq!(b.output_scroll(), 1);
        // Selection did not move.
        assert_eq!(b.selected(), 0);
    }

    #[test]
    fn output_scroll_stops_at_the_last_line() {
        let mut b = BlockBrowser::new(vec![block(1, "seq", 0, "1\n2\n3")]);
        b.on_key(key(KeyCode::Tab));
        b.on_key(key(KeyCode::Char('G')));
        assert_eq!(b.output_scroll(), 2);
        assert_eq!(b.on_key(key(KeyCode::Char('j'))), BrowserAction::Noop);
    }

    #[test]
    fn page_keys_scroll_by_the_pane_height() {
        let output: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let mut b = BlockBrowser::new(vec![block(1, "seq", 0, &output)]);
        b.set_output_height(20);

        b.on_key(ctrl('d'));
        assert_eq!(b.output_scroll(), 20);
        b.on_key(ctrl('u'));
        assert_eq!(b.output_scroll(), 0);
    }

    #[test]
    fn changing_selection_resets_the_output_scroll() {
        let mut b = BlockBrowser::new(vec![
            block(1, "a", 0, "1\n2\n3\n4\n5"),
            block(2, "b", 0, "x\ny\nz"),
        ]);
        b.on_key(key(KeyCode::Tab));
        b.on_key(key(KeyCode::Char('j')));
        assert_eq!(b.output_scroll(), 1);

        b.on_key(key(KeyCode::Tab)); // back to the list
        b.on_key(key(KeyCode::Char('j')));
        assert_eq!(b.output_scroll(), 0);
    }

    #[test]
    fn clamp_scroll_pulls_a_stale_offset_back_into_range() {
        let mut b = BlockBrowser::new(vec![block(1, "seq", 0, "1\n2\n3")]);
        b.on_key(key(KeyCode::Tab));
        b.on_key(key(KeyCode::Char('G')));
        assert_eq!(b.output_scroll(), 2);

        b.clamp_scroll();
        assert!(b.output_scroll() <= 2);
    }

    #[test]
    fn fold_collapses_output_to_the_configured_line_count() {
        let output: String = (0..20).map(|i| format!("line {i}\n")).collect();
        let mut b = BlockBrowser::new(vec![block(1, "seq", 0, &output)]);

        let (lines, hidden) = b.output_lines();
        assert_eq!(lines.len(), 20);
        assert_eq!(hidden, 0);

        b.on_key(key(KeyCode::Char(' ')));
        assert!(b.is_folded());
        let (lines, hidden) = b.output_lines();
        assert_eq!(lines.len(), FOLDED_LINES);
        assert_eq!(hidden, 20 - FOLDED_LINES);

        b.on_key(key(KeyCode::Char(' ')));
        assert!(!b.is_folded());
    }

    #[test]
    fn short_output_is_not_folded_even_when_marked() {
        let mut b = BlockBrowser::new(vec![block(1, "x", 0, "one\ntwo")]);
        b.on_key(key(KeyCode::Char(' ')));
        let (lines, hidden) = b.output_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn full_screen_program_output_starts_folded() {
        // A vim-like redraw is almost entirely cursor positioning.
        let noisy: String = (0..40)
            .map(|row| format!("\x1b[{};1H\x1b[K~\n", row))
            .collect();
        let b = BlockBrowser::new(vec![block(1, "vim", 0, &noisy)]);
        assert!(b.is_folded());
    }

    #[test]
    fn plain_output_does_not_start_folded() {
        assert!(!sample().is_folded());
    }

    #[test]
    fn stream_toggle_cycles_and_selects_the_right_text() {
        let mut blk = block(1, "cmd", 1, "on stdout");
        blk.stderr = "on stderr".to_string();
        let mut b = BlockBrowser::new(vec![blk]);

        assert_eq!(b.stream(), OutputStream::Both);
        assert_eq!(b.output_lines().0, vec!["on stdout", "on stderr"]);

        b.on_key(key(KeyCode::Char('s')));
        assert_eq!(b.stream(), OutputStream::Stdout);
        assert_eq!(b.output_lines().0, vec!["on stdout"]);

        b.on_key(key(KeyCode::Char('s')));
        assert_eq!(b.stream(), OutputStream::Stderr);
        assert_eq!(b.output_lines().0, vec!["on stderr"]);

        b.on_key(key(KeyCode::Char('s')));
        assert_eq!(b.stream(), OutputStream::Both);
    }

    #[test]
    fn output_is_ansi_stripped_and_progress_collapsed() {
        let b = &mut BlockBrowser::new(vec![block(
            1,
            "build",
            0,
            "\x1b[32m1/3\r2/3\r3/3\x1b[0m\ndone\n",
        )]);
        assert_eq!(b.output_lines().0, vec!["3/3", "done"]);
    }

    #[test]
    fn enter_inserts_and_r_runs_the_command() {
        let mut b = sample();
        assert_eq!(
            b.on_key(key(KeyCode::Enter)),
            BrowserAction::Finish(BrowserOutcome::Insert("git status".to_string()))
        );
        assert_eq!(
            b.on_key(key(KeyCode::Char('r'))),
            BrowserAction::Finish(BrowserOutcome::Run("git status".to_string()))
        );
    }

    #[test]
    fn d_jumps_to_the_directory_the_block_ran_in() {
        let mut b = sample();
        assert_eq!(
            b.on_key(key(KeyCode::Char('d'))),
            BrowserAction::Finish(BrowserOutcome::Run("cd /repo".to_string()))
        );
    }

    #[test]
    fn d_quotes_directories_that_need_it() {
        let mut blk = block(1, "ls", 0, "");
        blk.cwd = Some("/tmp/my project".to_string());
        let mut b = BlockBrowser::new(vec![blk]);
        assert_eq!(
            b.on_key(key(KeyCode::Char('d'))),
            BrowserAction::Finish(BrowserOutcome::Run("cd '/tmp/my project'".to_string()))
        );
    }

    #[test]
    fn d_without_a_recorded_directory_does_nothing() {
        let mut blk = block(1, "ls", 0, "");
        blk.cwd = None;
        let mut b = BlockBrowser::new(vec![blk]);
        assert_eq!(b.on_key(key(KeyCode::Char('d'))), BrowserAction::Noop);
    }

    #[test]
    fn e_routes_explanation_through_the_blocks_builtin() {
        // An AI call cannot happen inside the synchronous closure, so it goes
        // back to the shell as a command.
        let mut b = sample();
        b.on_key(key(KeyCode::Char('j')));
        assert_eq!(
            b.on_key(key(KeyCode::Char('e'))),
            BrowserAction::Finish(BrowserOutcome::Run("blocks explain 2".to_string()))
        );
    }

    #[test]
    fn explain_numbers_against_the_unfiltered_list() {
        // `blocks explain N` indexes get_all_blocks(); using the position within
        // the filter would explain a different block entirely.
        let mut b = sample();
        b.on_key(key(KeyCode::Char('f'))); // only "cargo test" survives
        assert_eq!(b.matched(), 1);
        assert_eq!(b.selected(), 0);
        assert_eq!(b.selected_block().unwrap().command, "cargo test");

        // "cargo test" is the 2nd entry of the full list, not the 1st.
        assert_eq!(
            b.on_key(key(KeyCode::Char('e'))),
            BrowserAction::Finish(BrowserOutcome::Run("blocks explain 2".to_string()))
        );
    }

    #[test]
    fn c_copies_the_command_and_y_copies_the_output() {
        let mut b = sample();
        assert_eq!(
            b.on_key(key(KeyCode::Char('c'))),
            BrowserAction::Copy("git status".to_string())
        );
        assert_eq!(
            b.on_key(key(KeyCode::Char('y'))),
            BrowserAction::Copy("clean".to_string())
        );
    }

    #[test]
    fn y_with_no_output_does_nothing() {
        let mut b = BlockBrowser::new(vec![block(1, "cd /tmp", 0, "")]);
        assert_eq!(b.on_key(key(KeyCode::Char('y'))), BrowserAction::Noop);
    }

    #[test]
    fn q_and_esc_and_ctrl_c_quit() {
        for k in [key(KeyCode::Char('q')), key(KeyCode::Esc), ctrl('c')] {
            let mut b = sample();
            assert_eq!(b.on_key(k), BrowserAction::Finish(BrowserOutcome::Quit));
        }
    }

    #[test]
    fn help_opens_and_the_next_key_dismisses_it() {
        let mut b = sample();
        b.on_key(key(KeyCode::Char('?')));
        assert!(b.show_help());

        // Dismissing must not also trigger the key's normal action.
        assert_eq!(b.on_key(key(KeyCode::Char('r'))), BrowserAction::Redraw);
        assert!(!b.show_help());
    }

    #[test]
    fn empty_output_explains_why_rather_than_showing_a_blank_pane() {
        let b = BlockBrowser::new(vec![block(1, "cd /tmp", 0, "")]);
        assert!(b.empty_output_note().is_some());

        let b = BlockBrowser::new(vec![block(1, "ls", 0, "file")]);
        assert!(b.empty_output_note().is_none());
    }

    #[test]
    fn truncation_is_surfaced_so_the_tail_is_not_misread() {
        let b = BlockBrowser::new(vec![block(1, "big", 0, "... (truncated)\nlast lines")]);
        assert!(b.is_truncated());
        assert!(!sample().is_truncated());
    }

    #[test]
    fn an_empty_block_list_has_nothing_selected() {
        let b = BlockBrowser::new(Vec::new());
        assert!(b.is_empty());
        assert_eq!(b.matched(), 0);
        assert!(b.selected_block().is_none());
    }

    #[test]
    fn keys_on_an_empty_list_do_not_finish_with_a_command() {
        let mut b = BlockBrowser::new(Vec::new());
        for k in [
            key(KeyCode::Enter),
            key(KeyCode::Char('r')),
            key(KeyCode::Char('d')),
            key(KeyCode::Char('e')),
            key(KeyCode::Char('c')),
            key(KeyCode::Char('y')),
        ] {
            assert_eq!(b.on_key(k), BrowserAction::Noop);
        }
    }

    #[test]
    fn quote_path_only_quotes_when_needed() {
        assert_eq!(quote_path("/repo/src"), "/repo/src");
        assert_eq!(quote_path("/tmp/my project"), "'/tmp/my project'");
        assert_eq!(quote_path("/tmp/it's"), r"'/tmp/it'\''s'");
        assert_eq!(quote_path("/tmp/$HOME"), "'/tmp/$HOME'");
        assert_eq!(quote_path(""), "''");
    }

    #[test]
    fn status_message_clears_on_the_next_key() {
        let mut b = sample();
        b.set_status("copied");
        assert_eq!(b.status(), Some("copied"));
        b.on_key(key(KeyCode::Char('j')));
        assert!(b.status().is_none());
    }
}
