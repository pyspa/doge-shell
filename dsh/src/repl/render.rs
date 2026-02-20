use super::Repl;
use crate::input::{ColorType, display_width};
use crate::parser::{self, HighlightKind, Rule};
use anyhow::Result;
use crossterm::cursor::{self, MoveLeft};
use crossterm::queue;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor, Stylize};
use crossterm::terminal::{self, Clear, ClearType};
use pest::iterators::Pairs;
use std::io::Write;
use tracing::debug;

pub(crate) fn move_cursor_input_end<W: Write>(repl: &Repl<'_>, out: &mut W) {
    let prompt_display_width = repl.prompt_mark_width;
    // cache locally to avoid duplicate computation chains
    let input_cursor_width = repl.input.cursor_display_width();
    let mut cursor_display_pos = prompt_display_width + input_cursor_width;

    // bound to current terminal columns if available
    if repl.columns > 0 {
        cursor_display_pos = cursor_display_pos.min(repl.columns.saturating_sub(1));
    } else {
        cursor_display_pos = cursor_display_pos.min(1000);
    }

    // crossterm uses 0-based column indexing
    queue!(
        out,
        ResetColor,
        cursor::MoveToColumn(cursor_display_pos as u16)
    )
    .ok();
}

/// Move cursor relatively on the input line given previous and new display positions
pub(crate) fn move_cursor_relative(
    _repl: &Repl<'_>,
    out: &mut impl Write,
    prev_display_pos: usize,
    new_display_pos: usize,
) {
    if new_display_pos == prev_display_pos {
        return;
    }
    if new_display_pos > prev_display_pos {
        let delta = (new_display_pos - prev_display_pos) as u16;
        queue!(out, cursor::MoveRight(delta)).ok();
    } else {
        let delta = (prev_display_pos - new_display_pos) as u16;
        queue!(out, cursor::MoveLeft(delta)).ok();
    }
}

pub(crate) fn print_block_separator(repl: &Repl<'_>, out: &mut impl Write) {
    if !repl.input_preferences.block_separator {
        return;
    }

    // Get the current terminal width
    let cols = repl.columns as u16;
    if cols == 0 {
        return;
    }

    // Create a string of '─' characters to fill the terminal width
    let separator = "─".repeat(cols as usize);

    queue!(
        out,
        SetForegroundColor(Color::DarkGrey),
        Print(separator),
        ResetColor,
        Print("\r\n")
    )
    .ok();
}

pub(crate) fn print_prompt(repl: &mut Repl<'_>, out: &mut impl Write) {
    if !repl.multiline_buffer.is_empty() {
        let continuation_prompt = "..> ";
        out.write_all(continuation_prompt.as_bytes()).ok();
        repl.prompt_mark_cache = continuation_prompt.to_string();
        repl.prompt_mark_width = 4; // length of "..> "
        return;
    }

    // OSC 133 A: Prompt start
    out.write_all(b"\x1b]133;A\x1b\\").ok();

    // OSC 7 Directory Tracking (emit before hooks)
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(hostname) = nix::unistd::gethostname()
    {
        let hostname: std::ffi::OsString = hostname;
        let hostname_str = hostname.to_string_lossy().to_string();
        let cwd_str = cwd.to_string_lossy();
        // Format: \x1b]7;file://<hostname><pwd>\x1b\
        // Note: We skip full URL encoding for simplicity, assumes standard paths.
        let osc7 = format!("\x1b]7;file://{}{}\x1b\\", hostname_str, cwd_str);
        out.write_all(osc7.as_bytes()).ok();
    }

    // debug!("print_prompt called - full preprompt + mark redraw");

    // Execute pre-prompt hooks
    if let Err(e) = repl.shell.exec_pre_prompt_hooks() {
        debug!("Error executing pre-prompt hooks: {}", e);
    }

    // Update status and render preprompt (acquire write lock)
    // print_preprompt requires mutable access as it might invalidate cache
    let mut buffer = Vec::new();
    let new_mark;
    {
        let mut prompt = repl.prompt.write();
        prompt.update_status(repl.last_status, repl.last_duration);
        prompt.print_preprompt(&mut buffer);
        new_mark = prompt.mark.clone();
    }

    // Perform I/O without holding the lock
    out.write_all(&buffer).ok();
    out.write_all(b"\r\n").ok();

    // Update cached mark and width in case mark changed
    if repl.prompt_mark_cache != new_mark {
        repl.prompt_mark_cache = new_mark;
        repl.prompt_mark_width = display_width(&repl.prompt_mark_cache);
    }

    // draw mark only (defer flushing to caller for batching)
    out.write_all(b"\r").ok();
    out.write_all(repl.prompt_mark_cache.as_bytes()).ok();
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

    for token in tokens {
        if token.start >= token.end || token.end > len {
            continue;
        }
        let color = match token.kind {
            HighlightKind::Command => {
                let word = &input[token.start..token.end];
                if repl.command_is_valid(word) {
                    can_execute = true;
                    ColorType::CommandExists
                } else {
                    ColorType::CommandNotExists
                }
            }
            HighlightKind::Argument | HighlightKind::Bareword => ColorType::Argument,
            HighlightKind::Variable => ColorType::Variable,
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

pub fn print_input(
    repl: &mut Repl<'_>,
    out: &mut impl Write,
    reset_completion: bool,
    refresh_suggestion: bool,
) {
    // debug!("print_input called, reset_completion: {}", reset_completion);
    queue!(out, cursor::Hide).ok();
    let input = repl.input.as_str().to_owned();
    let _prompt_display_width = repl.prompt_mark_width; // cached at new()/print_prompt()

    let mut completion: Option<String> = None;
    if input.is_empty() || reset_completion {
        repl.input.completion = None;
        repl.input.color_ranges = None;
        repl.input.can_execute = false;
        repl.last_analyzed_input.clear();
        repl.last_analysis_result = None;
    } else {
        completion = repl.get_completion_from_history(&input);

        // Use cached analysis if input hasn't changed (fast path)
        let analysis = if repl.last_analyzed_input == input && repl.last_analysis_result.is_some() {
            repl.last_analysis_result.clone().unwrap()
        } else {
            let result = repl.analyze_input(&input, completion.clone());
            repl.last_analyzed_input = input.clone();
            repl.last_analysis_result = Some(result.clone());
            result
        };

        if let Some(c) = analysis.completion_full {
            repl.input.completion = Some(c);
        }
        if let Some(suffix) = analysis.completion {
            completion = Some(suffix);
        }

        repl.input.color_ranges = analysis.color_ranges;
        repl.input.can_execute = analysis.can_execute;
    }

    if completion.is_none() {
        if refresh_suggestion {
            repl.refresh_inline_suggestion();
        }
    } else {
        repl.suggestion_manager.clear();
    }

    // Auto-fix ghost text logic
    let mut ai_suggestion_text = None;
    if repl.input.as_str().is_empty() && repl.auto_fix_suggestion.is_some() {
        ai_suggestion_text = repl.auto_fix_suggestion.as_deref();
    }

    let ghost_suffix = if completion.is_none() {
        repl.suggestion_manager.suffix(&input)
    } else {
        None
    };

    let ai_pending_now = repl.suggestion_manager.engine.ai_pending();

    // Clear the current line and redraw prompt mark + input
    queue!(out, Print("\r"), Clear(ClearType::CurrentLine)).ok();

    // Only redraw the prompt mark (not the full preprompt)
    // Use cached prompt mark without re-locking prompt
    queue!(out, Print(repl.prompt_mark_cache.as_str())).ok();

    // OSC 133 B: Command start
    out.write_all(b"\x1b]133;B\x1b\\").ok();

    // Print the input
    repl.input.print(out, ghost_suffix.as_deref());

    if let Some(ai_fix) = ai_suggestion_text {
        // Render AI suggestion with a distinct color
        queue!(out, Print(ai_fix.with(Color::DarkGrey))).ok();
        let width = display_width(ai_fix);
        queue!(out, MoveLeft(width as u16)).ok();
    }

    // Hint for AI Smart Extensions
    if repl.detect_smart_pipe().is_some() || repl.detect_generative_command().is_some() {
        let hint = " ↹ Tab to expand";
        let hint_width = display_width(hint);
        // Only show if we have enough space (avoid overlapping with input)
        let input_visual_end = repl.prompt_mark_width + repl.input.display_width();

        if repl.columns > hint_width
            && repl.columns.saturating_sub(hint_width) > input_visual_end + 2
        {
            let col = repl.columns - hint_width;
            queue!(
                out,
                cursor::MoveToColumn(col as u16),
                Print(hint.with(Color::DarkGrey))
            )
            .ok();
        }
    } else if repl.detect_ai_pipe().is_some() {
        // Hint for AI Output Pipe
        let hint = " ↵ Enter to analyze";
        let hint_width = display_width(hint);
        let input_visual_end = repl.prompt_mark_width + repl.input.display_width();

        if repl.columns > hint_width
            && repl.columns.saturating_sub(hint_width) > input_visual_end + 2
        {
            let col = repl.columns - hint_width;
            queue!(
                out,
                cursor::MoveToColumn(col as u16),
                Print(hint.with(Color::DarkGrey))
            )
            .ok();
        }
    }

    if ai_pending_now {
        queue!(out, Print(" ⧗")).ok();
    }

    repl.ai_pending_shown = ai_pending_now;

    move_cursor_input_end(repl, out);

    if let Some(completion) = completion {
        repl.input.print_candidates(out, completion);
        // reuse cached cursor width implicitly via move_cursor_input_end recomputation; avoid extra heavy work here
        move_cursor_input_end(repl, out);
    }
    queue!(out, cursor::Show).ok();
}

/// Helper function to render the transient prompt
/// Extracted for testability
pub(crate) fn render_transient_prompt_to<W: Write>(
    out: &mut W,
    input: &crate::input::Input,
    input_width: usize,
    prompt_width: usize,
    cols: u16,
) -> Result<()> {
    // Calculate how many lines the prompt+input occupies
    // Note: Preprompt is always one extra line above
    let input_lines = (prompt_width + input_width) / (cols as usize);
    let total_lines = 1 + input_lines; // +1 for preprompt

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

    // OSC 133 B: Command start
    out.write_all(b"\x1b]133;B\x1b\\").ok();

    // Render the input with existing syntax highlighting
    input.print(out, None);

    queue!(out, cursor::Show).ok();
    out.flush().ok();
    Ok(())
}
