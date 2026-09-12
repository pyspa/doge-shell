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
mod tests;
