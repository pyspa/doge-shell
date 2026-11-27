use super::Repl;
use crate::input::{ColorType, display_width};
use crate::parser::{self, HighlightKind, Rule};
use anyhow::Result;
use crossterm::cursor;
use crossterm::queue;
use crossterm::style::{Color, Print, ResetColor, Stylize};
use crossterm::terminal::{self, Clear, ClearType};
use pest::iterators::Pairs;
use std::io::Write;
use tracing::debug;

fn restore_cursor_position(
    out: &mut impl Write,
    cursor: (usize, usize),
    columns: usize,
    drawn_row: usize,
) {
    let cursor_display_pos = cursor.0.min(columns.checked_sub(1).unwrap_or(1000));
    let move_up = drawn_row.saturating_sub(cursor.1);

    if move_up > 0 {
        queue!(out, cursor::MoveUp(move_up as u16)).ok();
    }

    queue!(
        out,
        ResetColor,
        cursor::MoveToColumn(cursor_display_pos as u16)
    )
    .ok();
}

/// Move cursor relatively on the input line given previous and new display positions
pub(crate) fn move_cursor_relative(
    repl: &mut Repl<'_>,
    out: &mut impl Write,
    prev_pos: (usize, usize),
    previous_cursor: usize,
) {
    let input = repl.input.as_str();
    let at_end = repl.input.cursor_at_end();
    let crossed_end = at_end != (previous_cursor == repl.input.len());
    let refresh_ghost = crossed_end
        && (at_end
            || repl.input.completion.is_some()
            || repl.ai_ui.suggestion_manager.active.is_some());
    if refresh_ghost
        || ((input.contains("|?") || input.contains("??"))
            && input_hint(repl, previous_cursor) != input_hint(repl, repl.input.cursor()))
    {
        repl.terminal_ui.last_drawn_cursor_y = prev_pos.1;
        print_input(repl, out, false, refresh_ghost && at_end);
        return;
    }
    let new_pos = repl
        .input
        .cursor_pos(repl.terminal_ui.columns, repl.terminal_ui.prompt_mark_width);
    let (prev_col, prev_y) = prev_pos;
    let (new_col, new_y) = new_pos;

    if new_y < prev_y {
        queue!(out, cursor::MoveUp((prev_y - new_y) as u16)).ok();
    } else if new_y > prev_y {
        queue!(out, cursor::MoveDown((new_y - prev_y) as u16)).ok();
    }

    if new_col != prev_col {
        queue!(out, cursor::MoveToColumn(new_col as u16)).ok();
    }
}

/// Number of terminal rows an already-ANSI-stripped preprompt occupies at
/// `columns` wide.
///
/// Anything that erases and re-emits the preprompt must move up by this much;
/// assuming one row leaves an orphaned fragment whenever it wraps.
pub(crate) fn preprompt_rows(plain: &str, columns: usize) -> usize {
    if columns == 0 {
        return 1;
    }
    plain
        .split('\n')
        .map(|segment| {
            let width = display_width(segment.trim_end_matches('\r'));
            // A segment exactly `columns` wide still occupies one row: the
            // terminal defers the wrap until the next character.
            width.div_ceil(columns).max(1)
        })
        .sum()
}

/// Draw a new prompt: emits shell-integration markers and runs pre-prompt hooks.
pub(crate) fn print_prompt(repl: &mut Repl<'_>, out: &mut impl Write) {
    print_prompt_inner(repl, out, true)
}

/// Redraw the prompt that is already on screen.
///
/// Unlike [`print_prompt`] this emits no OSC 133 A (which would open a bogus
/// command block in shell-integration-aware terminals, with no matching
/// OSC 133 D) and runs no pre-prompt hooks — a redraw is not a new prompt, and
/// user hooks must not fire on terminal resizes or background job notices.
///
/// The caller is responsible for erasing the old prompt first; see
/// [`print_above_prompt`].
pub(crate) fn redraw_prompt(repl: &mut Repl<'_>, out: &mut impl Write) {
    print_prompt_inner(repl, out, false)
}

fn print_prompt_inner(repl: &mut Repl<'_>, out: &mut impl Write, new_prompt: bool) {
    // A full prompt establishes a new input drawing origin. Any previous input
    // redraw height belongs to the old prompt and must not clear later output.
    repl.terminal_ui.last_drawn_cursor_y = 0;

    if !repl.state.multiline_buffer.is_empty() {
        let continuation_prompt = "..> ";
        out.write_all(continuation_prompt.as_bytes()).ok();
        repl.terminal_ui.prompt_mark_cache = continuation_prompt.to_string();
        repl.terminal_ui.prompt_mark_width = 4; // length of "..> "
        // Continuation mode draws no preprompt line.
        repl.terminal_ui.last_preprompt_plain = None;
        return;
    }

    if new_prompt {
        let cwd = std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned());
        let hostname = nix::unistd::gethostname()
            .ok()
            .map(|name| name.to_string_lossy().into_owned());
        out.write_all(&super::shell_integration::fresh_prompt(
            cwd.as_deref(),
            hostname.as_deref(),
        ))
        .ok();

        // Execute pre-prompt hooks
        if let Err(e) = repl.shell.exec_pre_prompt_hooks() {
            debug!("Error executing pre-prompt hooks: {}", e);
        }
    }

    // Update status and render preprompt (acquire write lock)
    // print_preprompt requires mutable access as it might invalidate cache
    let mut buffer = Vec::new();
    let new_mark;
    {
        let mut prompt = repl.terminal_ui.prompt.write();
        prompt.update_status(repl.state.last_status, repl.state.last_duration);
        prompt.print_preprompt(&mut buffer);
        new_mark = prompt.mark.clone();
    }

    repl.terminal_ui.last_preprompt_plain =
        Some(console::strip_ansi_codes(&String::from_utf8_lossy(&buffer)).into_owned());

    // Perform I/O without holding the lock
    out.write_all(&buffer).ok();
    out.write_all(b"\r\n").ok();

    // Update cached mark and width in case mark changed
    if repl.terminal_ui.prompt_mark_cache != new_mark {
        repl.terminal_ui.prompt_mark_cache = new_mark;
        repl.terminal_ui.prompt_mark_width = display_width(&repl.terminal_ui.prompt_mark_cache);
    }

    // draw mark only (defer flushing to caller for batching)
    out.write_all(b"\r").ok();
    out.write_all(repl.terminal_ui.prompt_mark_cache.as_bytes())
        .ok();
    if new_prompt {
        out.write_all(super::shell_integration::prompt_end()).ok();
    }
    // no out.flush() here
}

pub(crate) fn highlight_result_to_ranges(
    repl: &Repl<'_>,
    highlight: parser::HighlightResult,
    input: &str,
) -> (Vec<(usize, usize, ColorType)>, bool) {
    let mut tokens = highlight.tokens;
    let error = highlight.error;

    // Skip sort if already sorted (common case)
    let needs_sort = tokens.windows(2).any(|w| w[0].start > w[1].start);
    if needs_sort {
        tokens.sort_by_key(|token| token.start);
    }

    let mut ranges = Vec::with_capacity(tokens.len() + error.as_ref().map(|_| 1).unwrap_or(0));
    let mut can_execute = false;
    let len = input.len();

    let mut command_cache = crate::repl::input_analysis::CommandValidityCache::new();

    for token in tokens {
        if token.start >= token.end || token.end > len {
            continue;
        }
        let color = match token.kind {
            HighlightKind::Command => {
                let word = &input[token.start..token.end];
                if command_cache.is_valid(repl, word) {
                    can_execute = true;
                    ColorType::CommandExists
                } else {
                    ColorType::CommandNotExists
                }
            }
            HighlightKind::Argument | HighlightKind::Bareword => ColorType::Argument,
            HighlightKind::Variable => ColorType::Variable,
            HighlightKind::Assignment => ColorType::Assignment,
            HighlightKind::SingleQuoted => ColorType::SingleQuote,
            HighlightKind::DoubleQuoted => ColorType::DoubleQuote,
            HighlightKind::Redirect => ColorType::Redirect,
            HighlightKind::Pipe => ColorType::Pipe,
            HighlightKind::Operator => ColorType::Operator,
            HighlightKind::Background => ColorType::Background,
            HighlightKind::ProcSubstitution => ColorType::ProcSubst,
            HighlightKind::Error => ColorType::Error,
        };
        ranges.push((token.start, token.end, color));
    }

    if let Some(err) = error
        && err.start < err.end
        && err.end <= len
    {
        ranges.push((err.start, err.end, ColorType::Error));
    }

    (ranges, can_execute)
}

pub(crate) fn compute_color_ranges_from_pairs<'p>(
    repl: &Repl<'_>,
    pairs: Pairs<'p, Rule>,
    input: &str,
) -> (Vec<(usize, usize, ColorType)>, bool) {
    let highlight = parser::collect_highlight_tokens_from_pairs(pairs, input.len());
    highlight_result_to_ranges(repl, highlight, input)
}

/// Emit `lines` above the inline prompt, then redraw the prompt and the
/// in-progress input so the user's typing is preserved.
///
/// Used for asynchronous notices (finished background jobs) that arrive from
/// the REPL's background tick while the user may be mid-line.
///
/// Callers must guarantee no other full-screen UI owns the terminal. That holds
/// for the background tick: the completion grid only exists while
/// `CompletionInteraction::run` blocks on `event::read()`, which runs *inside*
/// `handle_key_event`, so the tick cannot fire concurrently with it.
pub(crate) fn print_above_prompt<W: Write>(repl: &mut Repl<'_>, out: &mut W, lines: &[String]) {
    // `columns == 0` means we never sized the terminal (non-tty); the same
    // guard `render_transient_prompt_to` uses.
    if lines.is_empty() || repl.terminal_ui.columns == 0 {
        return;
    }

    // Move back over the whole prompt so it can be re-emitted below the notice.
    // The preprompt wraps when it is wider than the terminal, so ask for the
    // row count at the current width rather than assuming a single line.
    let up = repl.terminal_ui.last_drawn_cursor_y + repl.preprompt_rows();

    queue!(out, cursor::Hide, cursor::MoveToColumn(0)).ok();
    if up > 0 {
        queue!(out, cursor::MoveUp(up as u16)).ok();
    }
    queue!(out, Clear(ClearType::FromCursorDown)).ok();
    // ED is not bounded by the DECSTBM scroll region, so that just wiped the
    // status line's reserved row too. Drop its dedup cache so the next refresh
    // repaints instead of deciding nothing changed.
    repl.terminal_ui.status_line.borrow_mut().invalidate();

    for line in lines {
        out.write_all(line.as_bytes()).ok();
        out.write_all(b"\r\n").ok();
    }

    // A redraw, not a new prompt: no OSC 133 A, no pre-prompt hooks.
    redraw_prompt(repl, out);
    // `refresh_suggestion = false`: a timer tick must not kick off AI backfill.
    print_input(repl, out, false, false);

    // The argument explanation is drawn below the input via
    // SavePosition/RestorePosition and was wiped by the Clear above. Nulling the
    // cache makes the 200ms debounce redraw it.
    repl.ai_ui.last_explanation = None;
}

pub fn print_input(
    repl: &mut Repl<'_>,
    out: &mut impl Write,
    reset_completion: bool,
    refresh_suggestion: bool,
) {
    // debug!("print_input called, reset_completion: {}", reset_completion);
    queue!(out, cursor::Hide).ok();

    // Extract values needed before any mutable borrow of repl
    let is_empty = repl.input.is_empty();
    let input_string = repl.input.as_str().to_owned(); // Must allocate here to avoid E0502 when calling &mut repl methods
    let _prompt_display_width = repl.terminal_ui.prompt_mark_width; // cached at new()/print_prompt()
    let history_match_ranges = repl.input.color_ranges.as_ref().and_then(|ranges| {
        let ranges: Vec<_> = ranges
            .iter()
            .copied()
            .filter(|(_, _, kind)| matches!(kind, ColorType::HistoryMatch))
            .collect();
        (!ranges.is_empty()).then_some(ranges)
    });

    if refresh_suggestion || !reset_completion {
        repl.refresh_inline_suggestion(refresh_suggestion);
    } else {
        repl.input.completion = None;
    }

    if is_empty {
        repl.input.color_ranges = None;
        repl.input.can_execute = false;
        repl.ai_ui.last_analyzed_input.clear();
    } else if reset_completion || repl.ai_ui.last_analyzed_input != input_string {
        let analysis = repl.analyze_input(&input_string);
        repl.input.color_ranges = analysis.color_ranges;
        repl.input.can_execute = analysis.can_execute;
        repl.ai_ui.last_analyzed_input.clone_from(&input_string);
    }

    if let Some(ranges) = history_match_ranges {
        merge_history_match_ranges(&mut repl.input.color_ranges, ranges);
    }

    // Auto-fix ghost text: the replacement (if any) plus a right-aligned
    // annotation saying why and how to accept it. Owned copies so the borrow
    // does not outlive the mutable field updates below.
    let auto_fix = if is_empty {
        repl.ai_ui.auto_fix_suggestion.as_ref().map(|fix| {
            (
                fix.has_fix().then(|| fix.replacement.clone()),
                super::failure_hint::format_hint_annotation(fix.title.as_deref(), fix.has_fix()),
            )
        })
    } else {
        None
    };

    let ai_pending_now = repl.inline_ai_pending();

    // Clear the current line and redraw prompt mark + input
    if repl.terminal_ui.last_drawn_cursor_y > 0 {
        queue!(
            out,
            cursor::MoveUp(repl.terminal_ui.last_drawn_cursor_y as u16)
        )
        .ok();
    }
    queue!(out, Print("\r"), Clear(ClearType::FromCursorDown)).ok();

    // Only redraw the prompt mark (not the full preprompt)
    // Use cached prompt mark without re-locking prompt
    queue!(out, Print(repl.terminal_ui.prompt_mark_cache.as_str())).ok();

    // Set new cursor Y
    let cursor_pos = repl
        .input
        .cursor_pos(repl.terminal_ui.columns, repl.terminal_ui.prompt_mark_width);
    repl.terminal_ui.last_drawn_cursor_y = cursor_pos.1;

    // Draw exactly the suffix that the acceptance key would use, in place
    // while a terminal at its right margin still has a pending wrap.
    let inline = auto_fix
        .as_ref()
        .and_then(|(fix, _)| fix.as_deref())
        .map(|text| (text, Color::DarkGrey))
        .or_else(|| {
            repl.inline_suggestion_suffix().map(|suffix| {
                let color = if repl.completion_suffix().is_some() {
                    repl.input.completion_color()
                } else {
                    repl.input.ghost_color()
                };
                (suffix, color)
            })
        });
    repl.input.print_with_suffix(out, inline);
    let suffix = inline.map_or("", |(text, _)| text);
    let columns = repl.terminal_ui.columns;
    let (end_column, end_row) = if repl.input.cursor_at_end() {
        cursor_pos
    } else {
        crate::input::editor::visual_position(
            &input_string,
            columns,
            repl.terminal_ui.prompt_mark_width,
        )
    };
    let (suffix_column, suffix_rows) =
        crate::input::editor::visual_position(suffix, columns, end_column);
    let mut drawn_row = end_row + suffix_rows;
    let pending = if ai_pending_now { " ⧗" } else { "" };
    let hint_extra_width = display_width(suffix) + display_width(pending);
    if ai_pending_now {
        queue!(out, Print(pending)).ok();
        drawn_row += crate::input::editor::visual_position(pending, columns, suffix_column).1;
    }

    if let Some((_, annotation)) = auto_fix.as_ref() {
        render_hint_if_room(repl, out, annotation, hint_extra_width);
    }

    let path_hint = if repl.completion_ui.path_feedback.is_active() {
        repl.completion_ui.path_feedback.text(
            repl.input.as_str(),
            repl.input.cursor(),
            repl.shell
                .environment
                .read()
                .completion_state
                .path_generation,
            crate::completion::generators::system::system_command_scan_status(),
        )
    } else {
        None
    };
    if let Some(hint) = input_hint(repl, repl.input.cursor()) {
        render_hint_if_room(repl, out, hint.text(), hint_extra_width);
    } else if auto_fix.is_none()
        && !ai_pending_now
        && let Some(hint) = path_hint
        && !input_string.contains('\n')
        && suffix_rows == 0
    {
        render_hint_if_room(repl, out, hint, hint_extra_width);
    }

    repl.ai_ui.ai_pending_shown = ai_pending_now;

    restore_cursor_position(out, cursor_pos, columns, drawn_row);
    queue!(out, cursor::Show).ok();
}

fn merge_history_match_ranges(
    color_ranges: &mut Option<Vec<(usize, usize, ColorType)>>,
    history_ranges: Vec<(usize, usize, ColorType)>,
) {
    let mut merged = Vec::new();

    for (start, end, kind) in color_ranges.take().unwrap_or_default() {
        if matches!(kind, ColorType::HistoryMatch) {
            continue;
        }

        let mut segments = vec![(start, end, kind)];
        for &(match_start, match_end, _) in &history_ranges {
            let mut next = Vec::new();
            for (seg_start, seg_end, seg_kind) in segments {
                if match_end <= seg_start || match_start >= seg_end {
                    next.push((seg_start, seg_end, seg_kind));
                    continue;
                }
                if seg_start < match_start {
                    next.push((seg_start, match_start, seg_kind));
                }
                if match_end < seg_end {
                    next.push((match_end, seg_end, seg_kind));
                }
            }
            segments = next;
        }
        merged.extend(segments);
    }

    merged.extend(history_ranges);
    merged.sort_by_key(|(start, _, _)| *start);
    *color_ranges = (!merged.is_empty()).then_some(merged);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputHint {
    Expand,
    Analyze,
}

impl InputHint {
    fn text(self) -> &'static str {
        match self {
            Self::Expand => " ↹ Tab to expand",
            Self::Analyze => " ↵ Enter to analyze",
        }
    }
}

fn input_hint(repl: &Repl<'_>, line_cursor: usize) -> Option<InputHint> {
    use crate::ai_task_input::{TabAction, tab_action};
    let input = repl.state.logical_input(repl.input.as_str());
    let cursor = repl.state.multiline_buffer.chars().count() + line_cursor;
    if matches!(
        tab_action(&input, cursor),
        TabAction::Generate(_) | TabAction::ExpandPipe { .. }
    ) {
        Some(InputHint::Expand)
    } else if crate::ai_task_input::output_pipe(&input).is_some()
        && crate::ai_task_input::classify(&input).is_none()
    {
        Some(InputHint::Analyze)
    } else {
        None
    }
}

/// Draw a right-aligned DarkGrey hint when it fits. `ghost_width` is the
/// display width of any ghost text drawn after the input; without it a hint
/// can overwrite the ghost on narrow terminals.
fn render_hint_if_room(repl: &Repl<'_>, out: &mut impl Write, hint: &str, ghost_width: usize) {
    let hint_width = display_width(hint);
    let input_visual_end =
        repl.terminal_ui.prompt_mark_width + repl.input.display_width() + ghost_width;

    if repl.terminal_ui.columns > hint_width
        && repl.terminal_ui.columns.saturating_sub(hint_width) > input_visual_end + 2
    {
        let col = repl.terminal_ui.columns - hint_width;
        queue!(
            out,
            cursor::MoveToColumn(col as u16),
            Print(hint.with(Color::DarkGrey))
        )
        .ok();
    }
}

/// Helper function to render the transient prompt
/// Extracted for testability
pub(crate) fn render_transient_prompt_to<W: Write>(
    out: &mut W,
    input: &crate::input::Input,
    prompt_width: usize,
    cols: u16,
) -> Result<()> {
    if cols == 0 {
        return Ok(());
    }

    // Move only from the current cursor line back to the preprompt. Width-only
    // division overcounts when the input ends exactly at the terminal edge.
    let (_, cursor_y) = input.cursor_pos(cols as usize, prompt_width);
    let total_lines = 1 + cursor_y; // +1 for preprompt

    queue!(
        out,
        cursor::Hide,
        cursor::MoveToColumn(0),
        cursor::MoveUp(total_lines as u16),
        terminal::Clear(ClearType::FromCursorDown)
    )
    .ok();

    // Print transient prompt symbol (Green ❯)
    // We use write! instead of print! to support the generic writer
    queue!(out, Print("❯".green()), Print(" ")).ok();

    // Render the input with existing syntax highlighting
    input.print(out, None);

    queue!(out, cursor::Show).ok();
    out.flush().ok();
    Ok(())
}

#[cfg(test)]
mod tests;
