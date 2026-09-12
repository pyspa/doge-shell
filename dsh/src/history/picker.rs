//! Interactive history picker for Ctrl-R.
//!
//! The scope/status/duration filters already existed on [`HistoryQuery`] but
//! were only reachable from the `history` builtin; Ctrl-R hardcoded a global,
//! unfiltered search and threw away every bit of metadata the entries carry.
//! This picker exposes those filters as live toggles and shows the exit status,
//! duration and directory alongside each command.
//!
//! All state transitions live in [`HistoryPicker::on_key`] and
//! [`HistoryPicker::recompute`], which touch no terminal, so the behaviour is
//! unit-testable. [`run`] is a thin ratatui driver on top.

use super::{Entry, EntryMatcher, HistoryQuery, HistoryScope, HistoryStatusFilter};
use crate::input::display_width;
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Commands slower than this count as "slow" for the Ctrl-T filter.
const SLOW_THRESHOLD_MS: u64 = 1000;

/// What the terminal driver should do after feeding a key to the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    /// State changed; draw again.
    Redraw,
    /// Nothing changed.
    Noop,
    /// Put this command in the input buffer. Never executed directly.
    Accept(String),
    /// Leave the input buffer untouched.
    Cancel,
}

/// One rendered row, already formatted for the current terminal width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    pub status: String,
    pub duration: String,
    pub age: String,
    pub cwd: String,
    pub command: String,
}

/// Which optional columns fit at a given width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnSet {
    pub status: bool,
    pub duration: bool,
    pub age: bool,
    pub cwd: bool,
}

/// Drop columns as the terminal narrows. The status glyph and the command
/// itself always survive; the directory is the first thing to go because it is
/// the widest and the least often needed.
pub fn columns_for_width(width: u16) -> ColumnSet {
    let width = width as usize;
    ColumnSet {
        status: width >= 20,
        duration: width >= 60,
        age: width >= 76,
        cwd: width >= 100,
    }
}

pub fn format_status(exit_code: Option<i32>) -> String {
    match exit_code {
        Some(0) => "✔".to_string(),
        Some(code) => format!("✘{}", code),
        // Imported history and entries recorded before the metadata columns
        // existed have no exit code at all.
        None => "·".to_string(),
    }
}

pub fn format_duration(duration_ms: Option<u64>) -> String {
    match duration_ms {
        None => "-".to_string(),
        Some(ms) if ms < 1000 => format!("{}ms", ms),
        Some(ms) if ms < 60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        Some(ms) => {
            let secs = ms / 1000;
            format!("{}m{}s", secs / 60, secs % 60)
        }
    }
}

/// Coarse "how long ago", in the widest unit that still reads as a number.
pub fn format_relative_time(when: i64, now: i64) -> String {
    // Clock skew (when > now) reads as "now" instead of a negative or
    // clamped delta.
    let Some(delta) = now.checked_sub(when) else {
        return "now".to_string();
    };
    match delta {
        d if d < 60 => "now".to_string(),
        d if d < 3600 => format!("{}m", d / 60),
        d if d < 86_400 => format!("{}h", d / 3600),
        d if d < 86_400 * 365 => format!("{}d", d / 86_400),
        d => format!("{}y", d / (86_400 * 365)),
    }
}

/// Home-relative, then truncated from the left so the leaf directory — the part
/// that identifies the entry — stays visible.
pub fn shorten_cwd(cwd: &str, home: Option<&str>, max: usize) -> String {
    let mut shortened = cwd.to_string();
    if let Some(home) = home
        && !home.is_empty()
        && let Some(rest) = cwd.strip_prefix(home)
    {
        shortened = if rest.is_empty() {
            "~".to_string()
        } else {
            format!("~{}", rest)
        };
    }

    if max == 0 || display_width(&shortened) <= max {
        return shortened;
    }

    // Not hardcoded to 1: this codebase measures with `width_cjk`, under which
    // the ellipsis is an ambiguous-width character and reports 2.
    let ellipsis_width = display_width(ELLIPSIS);
    if max <= ellipsis_width {
        return tail_within(&shortened, max);
    }
    format!(
        "{}{}",
        ELLIPSIS,
        tail_within(&shortened, max - ellipsis_width)
    )
}

const ELLIPSIS: &str = "…";

/// The last `budget` display columns of `text`, never splitting a character.
fn tail_within(text: &str, budget: usize) -> String {
    let mut kept: Vec<char> = Vec::new();
    let mut width = 0usize;
    for ch in text.chars().rev() {
        let w = display_width(&ch.to_string());
        if width + w > budget {
            break;
        }
        width += w;
        kept.push(ch);
    }
    kept.reverse();
    kept.into_iter().collect()
}

/// Render a command for a single-row table cell.
///
/// Multi-line entries would otherwise break the row layout; the raw command is
/// still what [`PickerAction::Accept`] returns.
fn flatten_command(command: &str) -> String {
    command.replace("\r\n", "⏎").replace(['\n', '\r'], "⏎")
}

pub struct HistoryPicker {
    entries: Vec<Entry>,
    /// Lowercased command text, parallel to `entries`, so filtering on each
    /// keystroke does not re-lowercase the whole snapshot.
    normalized: Vec<String>,
    base: HistoryQuery,
    query: String,
    scope: HistoryScope,
    status: HistoryStatusFilter,
    slow_only: bool,
    /// Indices into `entries`, newest first.
    filtered: Vec<usize>,
    selected: usize,
    home: Option<String>,
    now: i64,
    /// Terminal width, for column selection and cwd truncation.
    width: u16,
}

impl HistoryPicker {
    pub fn new(entries: Vec<Entry>, base: HistoryQuery, initial_query: String, now: i64) -> Self {
        let normalized = entries.iter().map(|e| e.entry.to_lowercase()).collect();
        let mut picker = Self {
            entries,
            normalized,
            base,
            query: initial_query,
            scope: HistoryScope::default(),
            status: HistoryStatusFilter::default(),
            slow_only: false,
            filtered: Vec::new(),
            selected: 0,
            home: dirs::home_dir().map(|p| p.to_string_lossy().into_owned()),
            now,
            width: 80,
        };
        picker.recompute();
        picker
    }

    pub fn set_width(&mut self, width: u16) {
        self.width = width;
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn total(&self) -> usize {
        self.entries.len()
    }

    pub fn matched(&self) -> usize {
        self.filtered.len()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    fn current_query(&self) -> HistoryQuery {
        HistoryQuery {
            text: if self.query.is_empty() {
                None
            } else {
                Some(self.query.clone())
            },
            scope: self.scope,
            status: self.status,
            min_duration_ms: self.slow_only.then_some(SLOW_THRESHOLD_MS),
            limit: None,
            ..self.base.clone()
        }
    }

    /// Rebuild the match set and keep the selection inside it.
    fn recompute(&mut self) {
        let query = self.current_query();
        let matcher = EntryMatcher::new(&query);
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter(|(index, entry)| matcher.matches(entry, Some(self.normalized[*index].as_str())))
            .map(|(index, _)| index)
            .collect();

        // A narrowing filter can leave the selection past the end.
        self.selected = match self.filtered.len() {
            0 => 0,
            len => self.selected.min(len - 1),
        };
    }

    fn move_selection(&mut self, delta: isize) -> PickerAction {
        if self.filtered.is_empty() {
            return PickerAction::Noop;
        }
        let last = self.filtered.len() - 1;
        let next = if delta < 0 {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            // Saturating: Home/End pass a deliberately huge delta.
            self.selected.saturating_add(delta as usize).min(last)
        };
        if next == self.selected {
            return PickerAction::Noop;
        }
        self.selected = next;
        PickerAction::Redraw
    }

    /// Rows the list can show, used for paging.
    fn page(&self) -> usize {
        10
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PickerAction {
        const CTRL: KeyModifiers = KeyModifiers::CONTROL;

        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), CTRL) | (KeyCode::Char('g'), CTRL) => {
                PickerAction::Cancel
            }

            (KeyCode::Enter, _) | (KeyCode::Tab, _) => match self.selected_entry() {
                // Accepting nothing would clear the user's input buffer.
                None => PickerAction::Noop,
                Some(entry) => PickerAction::Accept(entry.entry.clone()),
            },

            // Cycle the filters that `HistoryQuery` already supported.
            (KeyCode::Char('r'), CTRL) => {
                self.scope = next_scope(self.scope);
                self.recompute();
                PickerAction::Redraw
            }
            (KeyCode::Char('s'), CTRL) => {
                self.status = next_status(self.status);
                self.recompute();
                PickerAction::Redraw
            }
            (KeyCode::Char('t'), CTRL) => {
                self.slow_only = !self.slow_only;
                self.recompute();
                PickerAction::Redraw
            }

            (KeyCode::Up, _) | (KeyCode::Char('p'), CTRL) => self.move_selection(-1),
            (KeyCode::Down, _) | (KeyCode::Char('n'), CTRL) => self.move_selection(1),
            (KeyCode::PageUp, _) => self.move_selection(-(self.page() as isize)),
            (KeyCode::PageDown, _) => self.move_selection(self.page() as isize),
            (KeyCode::Home, _) => self.move_selection(isize::MIN / 2),
            (KeyCode::End, _) => self.move_selection(isize::MAX / 2),

            (KeyCode::Backspace, _) => {
                if self.query.pop().is_none() {
                    return PickerAction::Noop;
                }
                self.recompute();
                PickerAction::Redraw
            }
            (KeyCode::Char('u'), CTRL) => {
                if self.query.is_empty() {
                    return PickerAction::Noop;
                }
                self.query.clear();
                self.recompute();
                PickerAction::Redraw
            }

            (KeyCode::Char(ch), m) if !m.contains(CTRL) && !m.contains(KeyModifiers::ALT) => {
                self.query.push(ch);
                self.recompute();
                PickerAction::Redraw
            }

            _ => PickerAction::Noop,
        }
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.filtered.get(self.selected).map(|i| &self.entries[*i])
    }

    /// Status line describing the active filters and the match count.
    ///
    /// `(last run)` is not decoration: the schema has a UNIQUE index on the
    /// command text, so there is one row per distinct command carrying only its
    /// most recent metadata. `scope:cwd` therefore means "whose last run was
    /// here", not "ever run here".
    pub fn header(&self) -> String {
        let scope = match self.scope {
            HistoryScope::Global => "global",
            HistoryScope::Session => "session",
            HistoryScope::Cwd => "cwd",
            HistoryScope::Project => "project",
        };
        let status = match self.status {
            HistoryStatusFilter::Any => "any",
            HistoryStatusFilter::Success => "success",
            HistoryStatusFilter::Failure => "failure",
        };
        format!(
            "scope:{}  status:{}  slow:{}  ({}/{}, last run)",
            scope,
            status,
            if self.slow_only { "on" } else { "off" },
            self.filtered.len(),
            self.entries.len(),
        )
    }

    pub fn rows(&self) -> Vec<PickerRow> {
        let columns = columns_for_width(self.width);
        // Leave the rest of the line for the command itself.
        let cwd_budget = (self.width as usize / 4).clamp(8, 30);

        self.filtered
            .iter()
            .map(|index| {
                let entry = &self.entries[*index];
                PickerRow {
                    status: if columns.status {
                        format_status(entry.exit_code)
                    } else {
                        String::new()
                    },
                    duration: if columns.duration {
                        format_duration(entry.duration_ms)
                    } else {
                        String::new()
                    },
                    age: if columns.age {
                        format_relative_time(entry.when, self.now)
                    } else {
                        String::new()
                    },
                    cwd: if columns.cwd {
                        shorten_cwd(
                            entry.cwd.as_deref().unwrap_or("-"),
                            self.home.as_deref(),
                            cwd_budget,
                        )
                    } else {
                        String::new()
                    },
                    command: flatten_command(&entry.entry),
                }
            })
            .collect()
    }
}

fn next_scope(scope: HistoryScope) -> HistoryScope {
    match scope {
        HistoryScope::Global => HistoryScope::Session,
        HistoryScope::Session => HistoryScope::Cwd,
        HistoryScope::Cwd => HistoryScope::Project,
        HistoryScope::Project => HistoryScope::Global,
    }
}

fn next_status(status: HistoryStatusFilter) -> HistoryStatusFilter {
    match status {
        HistoryStatusFilter::Any => HistoryStatusFilter::Failure,
        HistoryStatusFilter::Failure => HistoryStatusFilter::Success,
        HistoryStatusFilter::Success => HistoryStatusFilter::Any,
    }
}

/// Drive the picker on the alternate screen.
///
/// Returns the accepted command, or `None` when cancelled or when there is no
/// terminal to draw on.
pub fn run(mut picker: HistoryPicker) -> Result<Option<String>> {
    use crossterm::event::{self, Event};
    use crossterm::execute;
    use crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use std::io::{self, IsTerminal};

    // Bail out before touching raw mode when there is nothing to draw on.
    if !io::stdout().is_terminal() {
        return Ok(None);
    }
    let Ok((cols, rows)) = crossterm::terminal::size() else {
        return Ok(None);
    };
    if cols < 20 || rows < 5 {
        return Ok(None);
    }
    picker.set_width(cols);

    struct TerminalGuard;
    impl Drop for TerminalGuard {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            let _ = execute!(io::stdout(), crossterm::cursor::Show);
        }
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // ratatui diffs against an all-blank buffer, so its first draw paints only
    // non-blank cells; clear so nothing shows through the gaps on terminals
    // that hand out a dirty alternate screen. Done here rather than via
    // `Terminal::clear`, which snapshots the cursor with a DSR round-trip that
    // can hang when the terminal is slow to answer.
    execute!(
        stdout,
        EnterAlternateScreen,
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
    )?;
    // Restores the terminal even if drawing panics.
    let _guard = TerminalGuard;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|frame| draw(frame, &picker))?;

        match event::read()? {
            Event::Key(key) if key.kind == event::KeyEventKind::Press => match picker.on_key(key) {
                PickerAction::Accept(command) => return Ok(Some(command)),
                PickerAction::Cancel => return Ok(None),
                PickerAction::Redraw | PickerAction::Noop => {}
            },
            Event::Resize(cols, _) => picker.set_width(cols),
            _ => {}
        }
    }
}

fn draw(frame: &mut ratatui::Frame, picker: &HistoryPicker) {
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("history> ", Style::default().fg(Color::Cyan)),
            Span::raw(picker.query()),
        ])),
        chunks[0],
    );

    let rows = picker.rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| {
            let mut spans = Vec::new();
            if !row.status.is_empty() {
                let color = if row.status.starts_with('✘') {
                    Color::Red
                } else {
                    Color::Green
                };
                spans.push(Span::styled(
                    format!("{:<4}", row.status),
                    Style::default().fg(color),
                ));
            }
            if !row.duration.is_empty() {
                spans.push(Span::styled(
                    format!("{:>7}  ", row.duration),
                    Style::default().fg(Color::Yellow),
                ));
            }
            if !row.age.is_empty() {
                spans.push(Span::styled(
                    format!("{:>4}  ", row.age),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            if !row.cwd.is_empty() {
                spans.push(Span::styled(
                    format!("{:<30}  ", row.cwd),
                    Style::default().fg(Color::Blue),
                ));
            }
            spans.push(Span::raw(row.command.clone()));
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::TOP))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if picker.matched() > 0 {
        state.select(Some(picker.selected()));
    }
    frame.render_stateful_widget(list, chunks[1], &mut state);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                "{}   ^R scope  ^S status  ^T slow  Enter accept  Esc cancel",
                picker.header()
            ),
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[2],
    );
}

#[cfg(test)]
mod tests;
