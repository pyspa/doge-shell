use crate::input::{ColorType, Input};
use crate::repl::Repl;
use crate::repl::state::ReplControlFlow;
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use crossterm::cursor;
use crossterm::queue;
use dsh_frecency::ItemStats;

fn apply_history_item(input: &mut Input, item: &ItemStats) {
    let color_ranges: Vec<(usize, usize, crate::input::ColorType)> = item
        .match_index
        .iter()
        .filter_map(|&idx| char_index_range(&item.item, idx))
        .map(|(start, end)| (start, end, ColorType::HistoryMatch))
        .collect();
    input.reset_with_color_ranges(item.item.clone(), color_ranges);
}

fn apply_history_command(input: &mut Input, command: String, search_word: Option<&str>) {
    let color_ranges = search_word
        .map(|word| history_match_ranges(&command, word))
        .unwrap_or_default();

    if color_ranges.is_empty() {
        input.reset(command);
    } else {
        input.reset_with_color_ranges(command, color_ranges);
    }
}

fn char_index_range(input: &str, char_index: usize) -> Option<(usize, usize)> {
    input
        .char_indices()
        .nth(char_index)
        .map(|(start, ch)| (start, start + ch.len_utf8()))
}

fn history_match_ranges(input: &str, word: &str) -> Vec<(usize, usize, ColorType)> {
    if word.is_empty() {
        return Vec::new();
    }

    if word.chars().any(|ch| ch.is_uppercase()) {
        return input
            .match_indices(word)
            .map(|(start, matched)| (start, start + matched.len(), ColorType::HistoryMatch))
            .collect();
    }

    case_insensitive_match_ranges(input, word)
}

fn case_insensitive_match_ranges(input: &str, word: &str) -> Vec<(usize, usize, ColorType)> {
    let haystack: Vec<(usize, usize, String)> = input
        .char_indices()
        .map(|(start, ch)| (start, start + ch.len_utf8(), ch.to_lowercase().to_string()))
        .collect();
    let needle: Vec<String> = word
        .chars()
        .map(|ch| ch.to_lowercase().to_string())
        .collect();

    if needle.is_empty() || haystack.len() < needle.len() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut index = 0;
    while index + needle.len() <= haystack.len() {
        if haystack[index..index + needle.len()]
            .iter()
            .map(|(_, _, lower)| lower)
            .eq(needle.iter())
        {
            ranges.push((
                haystack[index].0,
                haystack[index + needle.len() - 1].1,
                ColorType::HistoryMatch,
            ));
            index += needle.len();
        } else {
            index += 1;
        }
    }

    ranges
}

pub(crate) fn handle_history_previous(repl: &mut Repl<'_>) {
    // If completion menu is active, use it for navigation
    if repl.completion.completion_mode() {
        if let Some(item) = repl.completion.backward() {
            apply_history_item(&mut repl.input, item);
        }
        return;
    }

    // Magic Up Arrow: fish-style contains history search
    if let Some(history_arc) = &repl.shell.cmd_history {
        // Try to lock history (non-blocking)
        if let Some(mut history) = history_arc.try_lock() {
            let input_str = repl.input.as_str().to_string();

            // Check if input is empty to reset search word
            if input_str.is_empty() {
                history.search_word = None;
            }

            // If we are at the start of history navigation (bottom), initialize search
            // Use at_end() to check if we are at the "newest" position
            if history.at_end() && !input_str.is_empty() {
                history.search_word = Some(input_str);
            }

            if let Some(cmd) = history.back() {
                let search_word = history.search_word.clone();
                apply_history_command(&mut repl.input, cmd, search_word.as_deref());
            }
        }
    }
}

pub(crate) fn handle_history_next(repl: &mut Repl<'_>) {
    if repl.completion.completion_mode() {
        if let Some(item) = repl.completion.forward() {
            apply_history_item(&mut repl.input, item);
        }
        return;
    }

    // Magic Down Arrow
    if let Some(history_arc) = &repl.shell.cmd_history
        && let Some(mut history) = history_arc.try_lock()
    {
        let input_str = repl.input.as_str().to_string();

        // Check if input is empty to reset search word
        if input_str.is_empty() {
            history.search_word = None;
        }

        // If already at end, we can't go forward
        if history.at_end() {
            return;
        }

        if let Some(cmd) = history.forward() {
            let search_word = history.search_word.clone();
            apply_history_command(&mut repl.input, cmd, search_word.as_deref());
        } else {
            // If forward() returns None, we are back at the prompt line (future)
            // Restore the original search prefix or clear input
            let saved_input = history.search_word.clone().unwrap_or_default();
            repl.input.reset(saved_input);
            history.search_word = None;
        }
    }
}

pub(crate) async fn handle_cursor_left(
    repl: &mut Repl<'_>,
    _prev_cursor_disp: usize,
) -> Result<ReplControlFlow> {
    if repl.input.cursor() > 0 {
        repl.input.completion = None;
        let prev_pos = repl.input.cursor_pos(repl.columns, repl.prompt_mark_width);
        repl.input.move_by(-1);
        let new_pos = repl.input.cursor_pos(repl.columns, repl.prompt_mark_width);
        repl.completion.clear();

        let mut renderer = TerminalRenderer::new();
        repl.move_cursor_relative(&mut renderer, prev_pos, new_pos);
        queue!(renderer, cursor::Show).ok();
        queue!(renderer, cursor::Show).ok(); // Duplicate in original, keeping it?
        renderer.flush().ok();
        Ok(ReplControlFlow::Continue)
    } else {
        Ok(ReplControlFlow::Continue)
    }
}

pub(crate) async fn handle_cursor_right(
    repl: &mut Repl<'_>,
    _prev_cursor_disp: usize,
) -> Result<ReplControlFlow> {
    if repl.input.cursor() < repl.input.len() {
        let prev_pos = repl.input.cursor_pos(repl.columns, repl.prompt_mark_width);
        repl.input.move_by(1);
        let new_pos = repl.input.cursor_pos(repl.columns, repl.prompt_mark_width);
        repl.completion.clear();

        let mut renderer = TerminalRenderer::new();
        repl.move_cursor_relative(&mut renderer, prev_pos, new_pos);
        queue!(renderer, cursor::Show).ok();
        renderer.flush().ok();
        Ok(ReplControlFlow::Continue)
    } else {
        Ok(ReplControlFlow::Continue)
    }
}

pub(crate) async fn handle_cursor_word_left(
    repl: &mut Repl<'_>,
    _prev_cursor_disp: usize,
) -> Result<ReplControlFlow> {
    let prev_pos = repl.input.cursor_pos(repl.columns, repl.prompt_mark_width);
    repl.input.move_word_left();
    let new_pos = repl.input.cursor_pos(repl.columns, repl.prompt_mark_width);
    repl.completion.clear();

    let mut renderer = TerminalRenderer::new();
    repl.move_cursor_relative(&mut renderer, prev_pos, new_pos);
    queue!(renderer, cursor::Show).ok();
    renderer.flush().ok();
    Ok(ReplControlFlow::Continue)
}

pub(crate) async fn handle_cursor_word_right(
    repl: &mut Repl<'_>,
    _prev_cursor_disp: usize,
) -> Result<ReplControlFlow> {
    let prev_pos = repl.input.cursor_pos(repl.columns, repl.prompt_mark_width);
    repl.input.move_word_right();
    let new_pos = repl.input.cursor_pos(repl.columns, repl.prompt_mark_width);
    repl.completion.clear();

    let mut renderer = TerminalRenderer::new();
    repl.move_cursor_relative(&mut renderer, prev_pos, new_pos);
    queue!(renderer, cursor::Show).ok();
    renderer.flush().ok();
    Ok(ReplControlFlow::Continue)
}

pub(crate) async fn handle_cursor_to_begin(
    repl: &mut Repl<'_>,
    _prev_cursor_disp: usize,
) -> Result<ReplControlFlow> {
    let prev_pos = repl.input.cursor_pos(repl.columns, repl.prompt_mark_width);
    repl.input.move_to_begin();
    let new_pos = repl.input.cursor_pos(repl.columns, repl.prompt_mark_width);

    let mut renderer = TerminalRenderer::new();
    repl.move_cursor_relative(&mut renderer, prev_pos, new_pos);
    renderer.flush().ok();
    Ok(ReplControlFlow::Continue)
}

pub(crate) async fn handle_cursor_to_end(
    repl: &mut Repl<'_>,
    _prev_cursor_disp: usize,
) -> Result<ReplControlFlow> {
    let prev_pos = repl.input.cursor_pos(repl.columns, repl.prompt_mark_width);
    repl.input.move_to_end();
    let new_pos = repl.input.cursor_pos(repl.columns, repl.prompt_mark_width);

    let mut renderer = TerminalRenderer::new();
    repl.move_cursor_relative(&mut renderer, prev_pos, new_pos);
    renderer.flush().ok();
    Ok(ReplControlFlow::Continue)
}
