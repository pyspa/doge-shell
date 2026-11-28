//! Legacy completion selection, request-scoped candidates, and Lisp adapters.
use crate::completion::display::{Candidate, CompletionConfig};
use crate::completion::framework::{
    CompletionFrameworkKind, CompletionRequest, select_with_framework_kind,
};
use crate::completion::fuzzy::fuzzy_match_score;
use crate::completion::integrated::GenericCompletion;
use crate::completion::skim_adapter::replace_space;
use crate::history::FrecencyHistory;
use crate::input::Input;
use crate::lisp::Value;
use crate::repl::Repl;
use dsh_frecency::SortMethod;
use skim::SkimItem;
use std::rc::Rc;
use std::sync::Arc;
use tracing::{debug, warn};

mod command;
mod current;
use current::{LegacyCompletionSnapshot, completion_from_current_with_prompt};

// Helper function to get current prompt and input for completion display
pub fn get_prompt_and_input_for_completion() -> (String, String) {
    // For backward compatibility, return reasonable defaults
    // In practice, the main completion path should use the version with explicit parameters
    ("$ ".to_string(), "".to_string())
}

pub fn default_completion_framework() -> CompletionFrameworkKind {
    match std::env::var("DOGESH_COMPLETION_FRAMEWORK") {
        Ok(value) if value.eq_ignore_ascii_case("skim") => CompletionFrameworkKind::Skim,
        Ok(value) if value.eq_ignore_ascii_case("inline") => CompletionFrameworkKind::Inline,
        _ => CompletionFrameworkKind::Inline,
    }
}

pub fn select_completion_items(
    items: Vec<Candidate>,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    if items.is_empty() {
        return CompletionSelection::None;
    }

    let config = CompletionConfig::default();
    select_completion_items_with_framework(items, query, prompt_text, input_text, config, framework)
}

pub fn select_completion_items_with_config(
    items: Vec<Candidate>,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    config: CompletionConfig,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    select_completion_items_with_framework(items, query, prompt_text, input_text, config, framework)
}

pub fn select_completion_items_with_framework(
    items: Vec<Candidate>,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    config: CompletionConfig,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    debug!(
        "select_completion_items_with_framework: items={}, query={:?}, prompt_text='{}', input_text='{}', framework={:?}",
        items.len(),
        query,
        prompt_text,
        input_text,
        framework
    );

    if items.is_empty() {
        debug!("No completion candidates found, returning None");
        return CompletionSelection::None;
    }

    // Fast path: if only one candidate, return it immediately without UI
    if items.len() == 1 {
        debug!("Single candidate fast path: returning {:?}", items[0]);
        return CompletionSelection::Selected(items[0].output().to_string());
    }

    let request = CompletionRequest::new(items, query, prompt_text, input_text, config);
    select_with_framework_kind(framework, request)
}

// Backward compatibility function
fn select_completion_items_simple(
    items: Vec<Candidate>,
    query: Option<&str>,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    // legacy path doesn't have prompt/input context
    let (prompt_text, input_text) = get_prompt_and_input_for_completion();

    select_completion_items_with_framework(
        items,
        query,
        &prompt_text,
        &input_text,
        CompletionConfig::default(),
        framework,
    )
}

/// Strip the `* ` marker that flags a selected entry in command output
/// (e.g. the current branch in `git branch --all`).
///
/// Only the exact `"* "` prefix is removed so a candidate that merely starts
/// with `*` (a glob result, `*tar`, ...) keeps its text.
fn strip_selected_marker(val: String) -> String {
    match val.strip_prefix("* ") {
        Some(trimmed) => trimmed.to_string(),
        None => val,
    }
}

/// Synchronous compatibility entry point; runtime Lisp completion uses the
/// cancellable async adapter with an explicit logical environment snapshot.
pub fn completion_from_cmd(
    input: String,
    query: Option<&str>,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    command::complete_sync(input, query, framework)
}

/// The shell argument being edited, before alias expansion. Redirection words
/// are not arguments, and only words left of the cursor can select a provider.
struct LegacyArgument {
    command: String,
    args: Vec<String>,
    index: usize,
}

impl LegacyArgument {
    fn at(source: &str, cursor: usize) -> Option<Self> {
        let cursor = super::command_segment::char_to_byte(source, cursor);
        let parsed = super::parser::CommandLineParser::new().parse(source, cursor);
        let source = parsed.source?;
        if source.suppressed || source.cursor_word <= source.command_word {
            return None;
        }
        let mut args = Vec::new();
        let mut cursor_arg = None;
        for (index, word) in source.argv() {
            if index == source.cursor_word {
                cursor_arg = Some(args.len());
            }
            args.push(super::shell_path::decode_literal_word(word));
        }
        Some(Self {
            command: parsed.command,
            args,
            index: cursor_arg?,
        })
    }

    fn lisp_definition<'a>(
        &self,
        definitions: &'a [super::AutoComplete],
    ) -> Option<&'a super::AutoComplete> {
        if definitions.is_empty() {
            return None;
        }
        // Lisp registers multiword targets with underscores. Compare only
        // after appending a whole shell word; underscores may also be literal.
        let mut key = self.command.clone();
        let mut selected = definitions.iter().find(|def| def.target == key);
        for word in &self.args[..self.index] {
            key.push('_');
            key.push_str(word);
            if let Some(definition) = definitions.iter().find(|def| def.target == key) {
                selected = Some(definition);
            }
        }
        selected
    }
}

pub async fn input_completion(
    input: &Input,
    repl: &Repl<'_>,
    query: Option<&str>,
    raw_token: Option<&str>,
    prompt_text: &str,
    input_text: &str,
) -> CompletionSelection {
    input_completion_at(
        input.as_str(),
        input.cursor(),
        repl,
        query,
        raw_token,
        prompt_text,
        input_text,
    )
    .await
}

pub(crate) async fn input_completion_at(
    source: &str,
    cursor: usize,
    repl: &Repl<'_>,
    query: Option<&str>,
    raw_token: Option<&str>,
    prompt_text: &str,
    input_text: &str,
) -> CompletionSelection {
    let framework = default_completion_framework();
    debug!("input_completion starting with query: {:?}", query);

    if let Some(argument) = LegacyArgument::at(source, cursor) {
        let (selection, preformatted) = completion_from_lisp_with_prompt(
            &argument,
            source,
            repl,
            query,
            prompt_text,
            input_text,
            framework,
        )
        .await;
        if !matches!(selection, CompletionSelection::None) {
            return if preformatted {
                selection
            } else {
                render_legacy_selection(selection, raw_token)
            };
        }

        let selection = match argument.command.as_str() {
            "cd" | "pushd" | "popd" => {
                dir_stack_candidates(&argument, repl, query, prompt_text, input_text, framework)
            }
            "z" => z_history_candidates(
                &argument,
                repl.shell.path_history.as_ref(),
                query,
                prompt_text,
                input_text,
                framework,
            ),
            _ => CompletionSelection::None,
        };
        if !matches!(selection, CompletionSelection::None) {
            return render_legacy_selection(selection, raw_token);
        }
    }

    let cwd = repl.terminal_ui.prompt.read().current_path().to_path_buf();
    let Some((filesystem, policy, _)) = repl
        .completion_ui
        .integrated_completion
        .filesystem_context_for_input(source, cursor, &cwd)
    else {
        return CompletionSelection::None;
    };
    let command = match policy {
        GenericCompletion::Files => false,
        GenericCompletion::Command => true,
        _ => return CompletionSelection::None,
    };
    let snapshot = LegacyCompletionSnapshot::capture(repl, filesystem.current_dir(), command);
    let selection = completion_from_current_with_prompt(
        repl,
        &snapshot,
        query,
        raw_token,
        prompt_text,
        input_text,
        framework,
    )
    .await;
    render_legacy_selection(selection, raw_token)
}

/// Legacy collectors/cache/UI carry raw words. Render selected output once;
/// deferred selectors carry Prepared candidates so Inline and Skim agree.
fn render_legacy_selection(
    selection: CompletionSelection,
    raw_token: Option<&str>,
) -> CompletionSelection {
    match selection {
        CompletionSelection::Selected(text) => {
            let candidates = crate::completion::shell_path::format_candidates_for_token(
                vec![Candidate::Basic(text)],
                raw_token,
            );
            CompletionSelection::Selected(candidates[0].output().to_string())
        }
        CompletionSelection::Interactive(items, query) => CompletionSelection::Interactive(
            crate::completion::shell_path::format_candidates_for_token(items, raw_token),
            query.map(|query| crate::completion::shell_path::normalize_path_token(&query)),
        ),
        other => other,
    }
}

async fn completion_from_lisp_with_prompt(
    argument: &LegacyArgument,
    source: &str,
    repl: &Repl<'_>,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    framework: CompletionFrameworkKind,
) -> (CompletionSelection, bool) {
    // Pass current input as argument to lisp function
    let lisp_engine = Rc::clone(&repl.shell.lisp_engine);
    let autocompletion = {
        let engine = lisp_engine.borrow();
        Rc::clone(&engine.env.borrow().autocompletion)
    };

    // Release the RefCell borrow before a command-backed definition awaits.
    let Some(compl) = argument.lisp_definition(&autocompletion.borrow()).cloned() else {
        return (CompletionSelection::None, false);
    };
    if let Some(func) = &compl.func {
        // run lisp func with input as argument
        let args = vec![Value::String(replace_space(source))];
        match lisp_engine.borrow().apply_func(func.to_owned(), args) {
            Ok(Value::List(list)) => {
                let mut items: Vec<Candidate> = Vec::new();
                for val in list.into_iter() {
                    items.push(Candidate::Basic(val.to_string()));
                }
                return (
                    select_completion_items(items, query, prompt_text, input_text, framework),
                    false,
                );
            }
            Ok(Value::String(str)) => {
                // A direct Lisp String is a preformatted replacement (e.g.
                // `hello\ world`); lists such as ["hello world"] are raw
                // literal words. Never decode or infer syntax from values.
                return (CompletionSelection::Selected(str), true);
            }
            Err(err) => {
                warn!("Lisp completion error: {err:?}");
            }
            _ => {}
        }
    } else if let Some(cmd) = &compl.cmd {
        let cwd = repl.terminal_ui.prompt.read().current_path().to_path_buf();
        let runtime = repl.shell.environment.read().command_runtime_snapshot(cwd);
        let res = command::complete_with_runtime(
            cmd.to_string(),
            runtime,
            query,
            prompt_text,
            input_text,
            framework,
        )
        .await;
        if let CompletionSelection::Selected(val) = res {
            return (
                CompletionSelection::Selected(strip_selected_marker(val)),
                false,
            );
        } else if matches!(
            res,
            CompletionSelection::Interactive(..)
                | CompletionSelection::Input(_)
                | CompletionSelection::Event(_)
                | CompletionSelection::Cancelled
        ) {
            return (res, false);
        }
    } else if let Some(items) = &compl.candidates {
        let items: Vec<Candidate> = items
            .iter()
            .map(|x| Candidate::Basic(x.trim().to_string()))
            .collect();
        return (
            select_completion_items(items, query, prompt_text, input_text, framework),
            false,
        );
    }
    (CompletionSelection::None, false)
}

/// Completes `+N` / `-N` directory-stack indices for `cd`, `pushd` and `popd`.
///
/// Only fires once the token already starts with `+` or `-`. Without that guard
/// a bare `popd <TAB>` would be answered here instead of by ordinary filesystem
/// completion, and the sign is what disambiguates "stack index" from "path".
pub fn completion_for_dir_stack(
    input: &Input,
    repl: &Repl,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    let Some(argument) = LegacyArgument::at(input.as_str(), input.cursor()) else {
        return CompletionSelection::None;
    };
    dir_stack_candidates(&argument, repl, query, prompt_text, input_text, framework)
}

fn dir_stack_candidates(
    argument: &LegacyArgument,
    repl: &Repl,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    if !matches!(argument.command.as_str(), "cd" | "pushd" | "popd") || argument.index != 0 {
        return CompletionSelection::None;
    }

    let token = query.unwrap_or("");
    let Some(sign) = token.chars().next().filter(|c| *c == '+' || *c == '-') else {
        return CompletionSelection::None;
    };
    // Anything after the sign must still be a partial number.
    if !token[1..].bytes().all(|b| b.is_ascii_digit()) {
        return CompletionSelection::None;
    }

    let stack = repl.shell.environment.read().dir_stack.clone();
    // Entry 0 is the current directory. `popd +0` drops it (same as bare
    // `popd`), but `cd +0` would be a no-op, so hide it there.
    let first = if argument.command == "cd" { 1 } else { 0 };

    let candidates: Vec<Candidate> = stack
        .iter()
        .enumerate()
        .skip(first)
        .filter(|(index, _)| format!("{sign}{index}").starts_with(token))
        .map(|(index, path)| Candidate::Item(format!("{sign}{index}"), path.clone()))
        .collect();

    select_completion_items(candidates, query, prompt_text, input_text, framework)
}

pub fn completion_for_z(
    input: &Input,
    repl: &Repl,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    completion_for_z_with_path_history(
        input,
        repl.shell.path_history.as_ref(),
        query,
        prompt_text,
        input_text,
        framework,
    )
}

pub(crate) fn completion_for_z_with_path_history(
    input: &Input,
    path_history: Option<&Arc<parking_lot::Mutex<FrecencyHistory>>>,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    let Some(argument) = LegacyArgument::at(input.as_str(), input.cursor()) else {
        return CompletionSelection::None;
    };
    z_history_candidates(
        &argument,
        path_history,
        query,
        prompt_text,
        input_text,
        framework,
    )
}

fn z_history_candidates(
    argument: &LegacyArgument,
    path_history: Option<&Arc<parking_lot::Mutex<FrecencyHistory>>>,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    if argument.command != "z" || !crate::proxy::z_history_operand(&argument.args, argument.index) {
        return CompletionSelection::None;
    }
    let pattern = query.map(super::shell_path::decode_literal_word);

    if let Some(history) = path_history {
        let items = history.lock().sorted(&SortMethod::Frecent);

        let candidates: Vec<Candidate> = items
            .iter()
            .filter(|item| {
                pattern
                    .as_deref()
                    .is_none_or(|q| fuzzy_match_score(&item.item, q).is_some())
            })
            .map(|i| Candidate::Item(i.item.clone(), format!("({:.1})", i.get_frecency())))
            .collect();

        return select_completion_items(candidates, query, prompt_text, input_text, framework);
    }
    CompletionSelection::None
}

use crate::completion::framework::CompletionSelection;

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
