//! Keys that put text into the buffer without running anything: `Alt+.`
//! (last argument), `Alt+;` (snippet), and `Alt+n` / `Alt+p` (placeholder
//! stops).

use crate::completion::Candidate;
use crate::repl::Repl;
use crate::repl::last_arg::LastArgState;
use crate::repl::placeholder::{self, PlaceholderState};
use tracing::warn;

/// How far back `Alt+.` looks. History is deduplicated per command string, so
/// this is plenty of distinct commands to walk.
const LAST_ARG_HISTORY_DEPTH: usize = 200;

/// `Alt+.` — insert the last argument of a previous command, walking further
/// back on each repeat. Returns whether the completion state should be reset.
pub(crate) fn handle_insert_last_argument(repl: &mut Repl<'_>) -> bool {
    if repl.state.last_arg.is_none() {
        repl.state.last_arg = build_state(repl);
    }

    let Some(state) = repl.state.last_arg.as_mut() else {
        return false;
    };
    let Some((start, end, argument)) = state.advance() else {
        // Out of history. Leave the last insertion in place rather than
        // clearing it, which is what readline does.
        return false;
    };

    repl.input.replace_range_chars(start, end, &argument);
    true
}

fn build_state(repl: &Repl<'_>) -> Option<LastArgState> {
    let history_arc = repl.shell.cmd_history.as_ref()?;
    // `try_lock` rather than `lock`: the background history writer holds this
    // briefly, and blocking the key handler on it would stall input.
    let Some(history) = history_arc.try_lock() else {
        warn!("Failed to acquire command history lock for Alt+. - lock is busy");
        return None;
    };
    let commands: Vec<String> = history
        .snapshot_entries(LAST_ARG_HISTORY_DEPTH)
        .into_iter()
        .map(|entry| entry.entry)
        .collect();
    drop(history);

    LastArgState::new(commands, repl.input.cursor())
}

/// `Alt+;` — pick a snippet and insert it at the cursor.
pub(crate) fn handle_insert_snippet(repl: &mut Repl<'_>) -> bool {
    let Some(manager) = crate::snippet::SnippetManager::open_default() else {
        return false;
    };
    let snippets = match manager.list() {
        Ok(snippets) => snippets,
        Err(err) => {
            warn!("Failed to list snippets: {}", err);
            return false;
        }
    };
    if snippets.is_empty() {
        return false;
    }

    // Output is the command (that is what gets inserted); the name and
    // description ride along as the detail line, and are what the grid's
    // filter matches on alongside the command text.
    let candidates: Vec<Candidate> = snippets
        .iter()
        .map(|snippet| {
            let detail = match snippet.description.as_deref().filter(|d| !d.is_empty()) {
                Some(description) => format!("{}: {}", snippet.name, description),
                None => snippet.name.clone(),
            };
            Candidate::Item(snippet.command.clone(), detail)
        })
        .collect();

    let prompt_text = repl.terminal_ui.prompt.read().mark.clone();
    let input_text = repl.input.as_str().to_string();

    // Same grid as TAB completion: it needs the bottom row back.
    let status_pause =
        crate::repl::status_line::StatusLinePause::new(repl.terminal_ui.status_line.clone());
    let Some(chosen) =
        crate::completion::framework::pick_candidate(candidates, &prompt_text, &input_text)
    else {
        return false;
    };

    drop(status_pause);

    if let Some(snippet) = snippets.iter().find(|s| s.command == chosen) {
        let _ = manager.record_use(&snippet.name);
    }

    let (text, spans) = placeholder::expand(&chosen);
    let offset = repl.input.cursor();
    repl.input.insert_str(&text);

    repl.state.placeholders = PlaceholderState::new(&spans, offset);
    if let Some(state) = repl.state.placeholders.as_ref() {
        repl.input.move_to(state.cursor());
    }

    true
}

/// `Alt+n` / `Alt+p` — move between the `{{placeholder}}` stops of the snippet
/// that was just inserted.
pub(crate) fn handle_placeholder_step(repl: &mut Repl<'_>, forward: bool) -> bool {
    let Some(state) = repl.state.placeholders.as_mut() else {
        return false;
    };
    let position = if forward { state.next() } else { state.prev() };
    repl.input.move_to(position);
    true
}
