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
    if repl.completion_ui.completion.completion_mode() {
        if let Some(item) = repl.completion_ui.completion.backward() {
            apply_history_item(&mut repl.input, item);
        }
        return;
    }

    if let Some(history_arc) = &repl.shell.cmd_history
        && let Some(mut history) = history_arc.try_lock()
    {
        if repl.history_draft.is_none() {
            history.reset_index();
            let input = repl.input.as_str();
            history.search_word = (!input.is_empty()).then(|| input.to_owned());
        }

        if let Some(cmd) = history.back() {
            let draft =
                repl.history_draft
                    .get_or_insert_with(|| super::super::history_nav::HistoryDraft {
                        text: repl.input.as_str().to_owned(),
                        cursor: repl.input.cursor(),
                        recalled_revision: repl.input.edit_revision(),
                    });
            apply_history_command(&mut repl.input, cmd, history.search_word.as_deref());
            draft.recalled_revision = repl.input.edit_revision();
        }
    }
}

pub(crate) fn handle_history_next(repl: &mut Repl<'_>) {
    if repl.completion_ui.completion.completion_mode() {
        if let Some(item) = repl.completion_ui.completion.forward() {
            apply_history_item(&mut repl.input, item);
        }
        return;
    }

    if let Some(draft) = repl.history_draft.as_mut()
        && let Some(history_arc) = &repl.shell.cmd_history
        && let Some(mut history) = history_arc.try_lock()
    {
        if let Some(cmd) = history.forward() {
            apply_history_command(&mut repl.input, cmd, history.search_word.as_deref());
            draft.recalled_revision = repl.input.edit_revision();
        } else if let Some(draft) = repl.history_draft.take() {
            repl.input.reset(draft.text);
            repl.input.move_to(draft.cursor);
            history.search_word = None;
        }
    }
}

pub(crate) async fn handle_cursor_left(
    repl: &mut Repl<'_>,
    _prev_cursor_disp: usize,
) -> Result<ReplControlFlow> {
    if repl.input.cursor() > 0 {
        let previous_cursor = repl.input.cursor();
        let prev_pos = repl
            .input
            .cursor_pos(repl.terminal_ui.columns, repl.terminal_ui.prompt_mark_width);
        repl.input.move_by(-1);
        repl.completion_ui.completion.clear();

        let mut renderer = TerminalRenderer::new();
        repl.move_cursor_relative(&mut renderer, prev_pos, previous_cursor);
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
        let previous_cursor = repl.input.cursor();
        let prev_pos = repl
            .input
            .cursor_pos(repl.terminal_ui.columns, repl.terminal_ui.prompt_mark_width);
        repl.input.move_by(1);
        repl.completion_ui.completion.clear();

        let mut renderer = TerminalRenderer::new();
        repl.move_cursor_relative(&mut renderer, prev_pos, previous_cursor);
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
    let previous_cursor = repl.input.cursor();
    let prev_pos = repl
        .input
        .cursor_pos(repl.terminal_ui.columns, repl.terminal_ui.prompt_mark_width);
    repl.input.move_word_left();
    repl.completion_ui.completion.clear();

    let mut renderer = TerminalRenderer::new();
    repl.move_cursor_relative(&mut renderer, prev_pos, previous_cursor);
    queue!(renderer, cursor::Show).ok();
    renderer.flush().ok();
    Ok(ReplControlFlow::Continue)
}

pub(crate) async fn handle_cursor_word_right(
    repl: &mut Repl<'_>,
    _prev_cursor_disp: usize,
) -> Result<ReplControlFlow> {
    let previous_cursor = repl.input.cursor();
    let prev_pos = repl
        .input
        .cursor_pos(repl.terminal_ui.columns, repl.terminal_ui.prompt_mark_width);
    repl.input.move_word_right();
    repl.completion_ui.completion.clear();

    let mut renderer = TerminalRenderer::new();
    repl.move_cursor_relative(&mut renderer, prev_pos, previous_cursor);
    queue!(renderer, cursor::Show).ok();
    renderer.flush().ok();
    Ok(ReplControlFlow::Continue)
}

pub(crate) async fn handle_cursor_to_begin(
    repl: &mut Repl<'_>,
    _prev_cursor_disp: usize,
) -> Result<ReplControlFlow> {
    let previous_cursor = repl.input.cursor();
    let prev_pos = repl
        .input
        .cursor_pos(repl.terminal_ui.columns, repl.terminal_ui.prompt_mark_width);
    repl.input.move_to_begin();

    let mut renderer = TerminalRenderer::new();
    repl.move_cursor_relative(&mut renderer, prev_pos, previous_cursor);
    renderer.flush().ok();
    Ok(ReplControlFlow::Continue)
}

pub(crate) async fn handle_cursor_to_end(
    repl: &mut Repl<'_>,
    _prev_cursor_disp: usize,
) -> Result<ReplControlFlow> {
    let previous_cursor = repl.input.cursor();
    let prev_pos = repl
        .input
        .cursor_pos(repl.terminal_ui.columns, repl.terminal_ui.prompt_mark_width);
    repl.input.move_to_end();

    let mut renderer = TerminalRenderer::new();
    repl.move_cursor_relative(&mut renderer, prev_pos, previous_cursor);
    renderer.flush().ok();
    Ok(ReplControlFlow::Continue)
}
