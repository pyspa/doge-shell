//! Legacy completion selection, request-scoped candidates, and Lisp adapters.
use crate::completion::cache::CompletionCache;
use crate::completion::commands::{deduplicate_candidates, get_commands};
use crate::completion::display::{Candidate, CompletionConfig};
use crate::completion::framework::{
    CompletionFrameworkKind, CompletionRequest, select_with_framework_kind,
};
use crate::completion::fuzzy::fuzzy_match_score;
use crate::completion::skim_adapter::replace_space;
use crate::history::FrecencyHistory;
use crate::input::Input;
use crate::lisp::Value;
use crate::repl::Repl;
use dsh_frecency::SortMethod;
use skim::SkimItem;
use std::collections::BTreeSet;
use std::fs::read_dir;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tracing::{debug, warn};

mod command;

const LEGACY_CACHE_TTL_MS: u64 = 3000;

static LEGACY_COMPLETION_CACHE: LazyLock<CompletionCache<Candidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(LEGACY_CACHE_TTL_MS)));

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

pub async fn input_completion(
    input: &Input,
    repl: &Repl<'_>,
    query: Option<&str>,
    raw_token: Option<&str>,
    prompt_text: &str,
    input_text: &str,
) -> CompletionSelection {
    let framework = default_completion_framework();
    // Main fallback completion function that tries multiple completion sources in sequence:
    // 1. Lisp-based completion (custom completion definitions)
    // 2. Current context completion (path completion, command completion from PATH)
    // 3. ChatGPT completion (if enabled and API key is set)

    debug!("input_completion starting with query: {:?}", query);

    // Try lisp-based completion first (custom completions defined by user)
    // Lisp callbacks stay local; command-backed definitions yield while the
    // bounded helper runs and return any interrupted editor event unchanged.
    let (res, preformatted) =
        completion_from_lisp_with_prompt(input, repl, query, prompt_text, input_text, framework)
            .await;
    if let CompletionSelection::Selected(_)
    | CompletionSelection::Interactive(..)
    | CompletionSelection::Input(_)
    | CompletionSelection::Event(_)
    | CompletionSelection::Cancelled = res
    {
        debug!("Lisp completion returned result: {:?}", res);
        return if preformatted {
            res
        } else {
            render_legacy_selection(res, raw_token)
        };
    }

    // Try directory-stack index completion (`cd -N`, `pushd +N`, `popd +N`)
    let stack_res =
        completion_for_dir_stack(input, repl, query, prompt_text, input_text, framework);
    if let CompletionSelection::Selected(_)
    | CompletionSelection::Interactive(..)
    | CompletionSelection::Input(_)
    | CompletionSelection::Event(_)
    | CompletionSelection::Cancelled = stack_res
    {
        return render_legacy_selection(stack_res, raw_token);
    }

    // Try z completion
    let z_res = completion_for_z(input, repl, query, prompt_text, input_text, framework);
    if let CompletionSelection::Selected(_)
    | CompletionSelection::Interactive(..)
    | CompletionSelection::Input(_)
    | CompletionSelection::Event(_)
    | CompletionSelection::Cancelled = z_res
    {
        return render_legacy_selection(z_res, raw_token);
    }

    // Try current context completion (files, directories, commands in PATH)
    let res =
        completion_from_current_with_prompt(input, repl, query, prompt_text, input_text, framework)
            .await;
    if let CompletionSelection::Selected(_)
    | CompletionSelection::Interactive(..)
    | CompletionSelection::Input(_)
    | CompletionSelection::Event(_)
    | CompletionSelection::Cancelled = res
    {
        debug!("Context completion returned result: {:?}", res);
        return render_legacy_selection(res, raw_token);
    }

    debug!("No completion candidates found from any source");

    // Return None if no completion sources provided any candidates
    // This is the "silent failure" behavior when no matches are found from any source
    // No error message is shown to user, maintaining the "no visible effect" behavior
    CompletionSelection::None
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
    input: &Input,
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
    let completion = autocompletion
        .borrow()
        .iter()
        .find(|compl| replace_space(input.as_str()).starts_with(&compl.target))
        .cloned();
    for compl in completion.iter() {
        let cmd_str = compl.target.to_string();
        // debug!("match cmd:'{}' in:'{}'", cmd_str, replace_space(input));
        if replace_space(input.as_str()).starts_with(cmd_str.as_str()) {
            if let Some(func) = &compl.func {
                // run lisp func with input as argument
                let args = vec![Value::String(replace_space(input.as_str()))];
                match lisp_engine.borrow().apply_func(func.to_owned(), args) {
                    Ok(Value::List(list)) => {
                        let mut items: Vec<Candidate> = Vec::new();
                        for val in list.into_iter() {
                            items.push(Candidate::Basic(val.to_string()));
                        }
                        return (
                            select_completion_items(
                                items,
                                query,
                                prompt_text,
                                input_text,
                                framework,
                            ),
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
                let Some(snapshot) = LegacyCompletionSnapshot::capture(repl) else {
                    return (CompletionSelection::None, false);
                };
                let res = command::complete_with_runtime(
                    cmd.to_string(),
                    snapshot.runtime,
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
            return (CompletionSelection::None, false);
        }
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
    let line = input.as_str();
    let command = line.split_whitespace().next().unwrap_or("");
    if !matches!(command, "cd" | "pushd" | "popd") {
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
    let first = if command == "cd" { 1 } else { 0 };

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
    let line = input.as_str();
    if !line.trim_start().starts_with("z ") && line.trim() != "z" {
        return CompletionSelection::None;
    }

    if let Some(history) = path_history {
        let history = history.lock();
        let items = history.sorted(&SortMethod::Frecent);

        let candidates: Vec<Candidate> = items
            .iter()
            .map(|i| Candidate::Item(i.item.clone(), format!("({:.1})", i.get_frecency())))
            .collect();

        return select_completion_items(candidates, query, prompt_text, input_text, framework);
    }
    CompletionSelection::None
}

async fn completion_from_current_with_prompt(
    _input: &Input,
    repl: &Repl<'_>,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    let Some(snapshot) = LegacyCompletionSnapshot::capture(repl) else {
        return CompletionSelection::None;
    };
    let Some(data) =
        cached_current_context_candidates(&snapshot, query.unwrap_or(""), &LEGACY_COMPLETION_CACHE)
            .await
    else {
        return CompletionSelection::None;
    };
    select_completion_items(
        data.items,
        data.selection_query.as_deref(),
        prompt_text,
        input_text,
        framework,
    )
}

/// The cache identity and collection inputs come from the same logical state.
/// This static cache also serves distinct Environment instances, so PATH's
/// actual values are included alongside its per-Environment generation.
struct LegacyCompletionSnapshot {
    runtime: dsh_types::process_runtime::CommandRuntimeSnapshot,
    home: Option<String>,
    path_generation: u64,
}

impl LegacyCompletionSnapshot {
    fn capture(repl: &Repl<'_>) -> Option<Self> {
        let cwd = std::env::current_dir().ok()?;
        Some(Self::from_environment(&repl.shell.environment.read(), cwd))
    }

    fn from_environment(environment: &crate::environment::Environment, cwd: PathBuf) -> Self {
        Self {
            runtime: environment.command_runtime_snapshot(cwd),
            home: environment.lookup_variable("HOME"),
            path_generation: environment.completion_state.path_generation,
        }
    }

    fn cache_scope(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut scope = std::collections::hash_map::DefaultHasher::new();
        self.runtime.current_dir().hash(&mut scope);
        self.home.hash(&mut scope);
        self.runtime.command_search_paths().hash(&mut scope);
        self.path_generation.hash(&mut scope);
        scope.finish()
    }

    fn expand_home(&self, query: &str) -> String {
        if (query == "~" || query.starts_with("~/"))
            && let Some(home) = &self.home
        {
            format!("{home}{}", &query[1..])
        } else {
            // Logical unset is final: do not rediscover process-global HOME.
            query.to_string()
        }
    }
}

async fn cached_current_context_candidates(
    snapshot: &LegacyCompletionSnapshot,
    query: &str,
    cache: &CompletionCache<Candidate>,
) -> Option<CurrentCompletionData> {
    let scope = snapshot.cache_scope();
    if let Some(hit) = cache.lookup_scoped(scope, query) {
        return Some(CurrentCompletionData {
            items: hit.candidates,
            selection_query: Some(query.to_string()),
        });
    }
    let data = collect_current_context_candidates(snapshot, query).await?;
    if !data.items.is_empty() {
        cache.set_scoped(scope, query.to_string(), data.items.clone());
    }
    Some(data)
}

#[derive(Debug, Clone)]
struct CurrentCompletionData {
    items: Vec<Candidate>,
    selection_query: Option<String>,
}

async fn collect_current_context_candidates(
    snapshot: &LegacyCompletionSnapshot,
    query_str: &str,
) -> Option<CurrentCompletionData> {
    let current_dir = snapshot.runtime.current_dir();
    let expanded = snapshot.expand_home(query_str);
    let expanded_path = Path::new(&expanded);
    let lookup_path = current_dir.join(expanded_path);

    // Determine the path to search and query substring
    let (search_path, path_query, only_path) = if !expanded.is_empty() && lookup_path.is_dir() {
        (expanded_path.to_path_buf(), String::new(), true)
    } else if let Some(parent) = expanded_path.parent() {
        let parent_buf = parent.to_path_buf();
        let has_parent = !parent_buf.as_os_str().is_empty();
        if let Some(file_name) = expanded_path.file_name()
            && let Some(file_name_str) = file_name.to_str()
        {
            (parent_buf, file_name_str.to_string(), has_parent)
        } else {
            (expanded_path.to_path_buf(), String::new(), has_parent)
        }
    } else {
        (PathBuf::new(), query_str.to_string(), false)
    };

    // Canonicalize the path for consistent resolution
    let lookup_dir = current_dir.join(&search_path);
    let canonical_path = lookup_dir.canonicalize().unwrap_or(lookup_dir);
    let canonical_str = canonical_path.display().to_string();
    let search_path_str = search_path.to_str()?.to_string();

    let mut items = if path_query.is_empty() {
        get_file_completions(&canonical_str, &search_path_str).await
    } else {
        get_file_completions_with_filter(&canonical_str, &search_path_str, Some(&path_query)).await
    };

    if !only_path {
        // Anchor relative/empty PATH entries before crossing the worker boundary.
        let paths: Vec<String> = snapshot
            .runtime
            .command_search_paths()
            .iter()
            .map(|path| current_dir.join(path).to_string_lossy().into_owned())
            .collect();
        let query_str = query_str.to_string();

        let mut command_items =
            tokio::task::spawn_blocking(move || get_commands(&paths, &query_str))
                .await
                .unwrap_or_default();

        items.append(&mut command_items);
        items = deduplicate_candidates(items);
    }

    Some(CurrentCompletionData {
        items,
        selection_query: Some(path_query),
    })
}

async fn get_file_completions(dir: &str, prefix: &str) -> Vec<Candidate> {
    debug!("get_file_completions: dir={}, prefix={}", dir, prefix);
    get_file_completions_with_filter(dir, prefix, None).await
}

async fn get_file_completions_with_filter(
    dir: &str,
    prefix: &str,
    filter_prefix: Option<&str>,
) -> Vec<Candidate> {
    let dir_owned = dir.to_string();
    let prefix_owned = prefix.to_string();
    let filter_prefix_owned = filter_prefix.map(|s| s.to_string());

    tokio::task::spawn_blocking(move || {
        get_file_completions_with_filter_sync(
            &dir_owned,
            &prefix_owned,
            filter_prefix_owned.as_deref(),
        )
    })
    .await
    .unwrap_or_default()
}

pub(super) fn get_file_completions_with_filter_sync(
    dir: &str,
    prefix: &str,
    filter_prefix: Option<&str>,
) -> Vec<Candidate> {
    debug!(
        "get_file_completions_with_filter: dir={}, prefix={}, filter_prefix={:?}",
        dir, prefix, filter_prefix
    );
    let mut candidates_set = BTreeSet::new();
    let prefix = if !prefix.is_empty() && !prefix.ends_with('/') {
        format!("{prefix}/")
    } else {
        prefix.to_string()
    };

    debug!("reading directory: {}", dir);
    match read_dir(dir) {
        Ok(entries) => {
            let mut entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
            entries.sort_by_key(|x| x.file_name());

            for entry in entries {
                // Handle potential errors when getting file name
                let file_name_os = entry.file_name();
                let file_name = match file_name_os.to_str() {
                    Some(name) => name,
                    None => {
                        // Skip entries with non-UTF-8 names
                        continue;
                    }
                };

                // Handle potential errors when getting file type
                let is_file = match entry.file_type() {
                    Ok(metadata) => metadata.is_file(),
                    Err(_) => {
                        // Skip entries where we can't determine file type
                        continue;
                    }
                };

                // Apply prefix filter if provided
                if let Some(filter) = filter_prefix
                    && fuzzy_match_score(file_name, filter).is_none()
                {
                    continue;
                }

                let candidate = if is_file {
                    Candidate::Item(format!("{prefix}{file_name}"), "(file)".to_string())
                } else {
                    Candidate::Item(format!("{prefix}{file_name}"), "(directory)".to_string())
                };

                // BTreeSet automatically handles deduplication
                candidates_set.insert(candidate);
            }
        }
        Err(_err) => {}
    }

    // Convert BTreeSet back to Vec to maintain the expected return type
    candidates_set.into_iter().collect()
}

use crate::completion::framework::CompletionSelection;

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
