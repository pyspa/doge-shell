use crate::completion::cache::CompletionCache;
use crate::completion::commands::{deduplicate_candidates, get_commands};
use crate::completion::display::{Candidate, CompletionConfig};
use crate::completion::framework::{
    CompletionFrameworkKind, CompletionRequest, select_with_framework_kind,
};
use crate::completion::fuzzy::fuzzy_match_score;
use crate::completion::skim_adapter::replace_space;
use crate::input::Input;
use crate::lisp::Value;
use crate::repl::Repl;
use dsh_frecency::SortMethod;
use skim::SkimItem;
use std::collections::BTreeSet;
use std::fs::read_dir;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tracing::{debug, warn};

const LEGACY_CACHE_TTL_MS: u64 = 3000;

static LEGACY_COMPLETION_CACHE: LazyLock<CompletionCache<Candidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(LEGACY_CACHE_TTL_MS)));

// Helper function to get current prompt and input for completion display
pub fn get_prompt_and_input_for_completion() -> (String, String) {
    // For backward compatibility, return reasonable defaults
    // In practice, the main completion path should use the version with explicit parameters
    ("$ ".to_string(), "".to_string())
}

pub fn last_word(s: &str) -> &str {
    s.split_whitespace().last().unwrap_or("")
}

pub fn default_completion_framework() -> CompletionFrameworkKind {
    match std::env::var("DSH_COMPLETION_FRAMEWORK") {
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

pub fn completion_from_cmd(
    input: String,
    query: Option<&str>,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    debug!("{} ", &input);
    match std::process::Command::new("sh")
        .arg("-c")
        .arg(input)
        .output()
    {
        Ok(output) => {
            if let Ok(out) = String::from_utf8(output.stdout) {
                let items: Vec<Candidate> = out
                    .lines()
                    .map(|x| x.trim())
                    .filter(|x| !x.is_empty())
                    .map(|x| Candidate::Basic(x.to_string()))
                    .collect();

                return select_completion_items_simple(items, query, framework);
            }
            CompletionSelection::None
        }
        _ => CompletionSelection::None,
    }
}

pub async fn input_completion(
    input: &Input,
    repl: &Repl<'_>,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
) -> CompletionSelection {
    let framework = if repl.input_preferences.use_floating_completion {
        CompletionFrameworkKind::Floating
    } else {
        default_completion_framework()
    };
    // Main fallback completion function that tries multiple completion sources in sequence:
    // 1. Lisp-based completion (custom completion definitions)
    // 2. Current context completion (path completion, command completion from PATH)
    // 3. ChatGPT completion (if enabled and API key is set)

    debug!("input_completion starting with query: {:?}", query);

    // Try lisp-based completion first (custom completions defined by user)
    // Lisp logic is synchronous but fast (unless user func is slow).
    // We keep it synchronous for now or wrap it if needed.
    let res =
        completion_from_lisp_with_prompt(input, repl, query, prompt_text, input_text, framework);
    if let CompletionSelection::Selected(_) | CompletionSelection::Interactive(..) = res {
        debug!("Lisp completion returned result: {:?}", res);
        return res;
    }

    // Try z completion
    let z_res = completion_for_z(input, repl, query, prompt_text, input_text, framework);
    if let CompletionSelection::Selected(_) | CompletionSelection::Interactive(..) = z_res {
        return z_res;
    }

    // Try current context completion (files, directories, commands in PATH)
    let res =
        completion_from_current_with_prompt(input, repl, query, prompt_text, input_text, framework)
            .await;
    if let CompletionSelection::Selected(_) | CompletionSelection::Interactive(..) = res {
        debug!("Context completion returned result: {:?}", res);
        return res;
    }

    debug!("No completion candidates found from any source");

    // Return None if no completion sources provided any candidates
    // This is the "silent failure" behavior when no matches are found from any source
    // No error message is shown to user, maintaining the "no visible effect" behavior
    CompletionSelection::None
}

fn completion_from_lisp_with_prompt(
    input: &Input,
    repl: &Repl,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    // Pass current input as argument to lisp function
    let lisp_engine = Rc::clone(&repl.shell.lisp_engine);
    let environment = Arc::clone(&lisp_engine.borrow().shell_env);

    // 1. completion from autocomplete
    for compl in environment.read().autocompletion.iter() {
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
                        return select_completion_items(
                            items,
                            query,
                            prompt_text,
                            input_text,
                            framework,
                        );
                    }
                    Ok(Value::String(str)) => {
                        return CompletionSelection::Selected(str);
                    }
                    Err(err) => {
                        warn!("Lisp completion error: {err:?}");
                    }
                    _ => {}
                }
            } else if let Some(cmd) = &compl.cmd {
                // run command
                let res = completion_from_cmd(cmd.to_string(), query, framework);
                if let CompletionSelection::Selected(val) = res {
                    if val.starts_with('*') {
                        // Skip the leading "* " prefix safely (handles non-ASCII)
                        let trimmed: String = val.chars().skip(2).collect();
                        return CompletionSelection::Selected(trimmed);
                    } else {
                        return CompletionSelection::Selected(val);
                    }
                } else if let CompletionSelection::Interactive(..) = res {
                    return res;
                }
            } else if let Some(items) = &compl.candidates {
                let items: Vec<Candidate> = items
                    .iter()
                    .map(|x| Candidate::Basic(x.trim().to_string()))
                    .collect();
                return select_completion_items(items, query, prompt_text, input_text, framework);
            }
            return CompletionSelection::None;
        }
    }
    CompletionSelection::None
}

pub fn completion_for_z(
    input: &Input,
    repl: &Repl,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    let line = input.as_str();
    if !line.trim_start().starts_with("z ") && line.trim() != "z" {
        return CompletionSelection::None;
    }

    if let Some(ref history) = repl.shell.path_history {
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
    debug!("completion_from_current_with_prompt: query={:?}", query);

    let query_str = query.unwrap_or("");
    // Check caching first
    if let Some(hit) = LEGACY_COMPLETION_CACHE.lookup(query_str) {
        debug!("Cache hit for query '{}'", query_str);
        LEGACY_COMPLETION_CACHE.extend_ttl(&hit.key);
        debug!("Returning cached results");
        return select_completion_items(
            hit.candidates.clone(),
            query,
            prompt_text,
            input_text,
            framework,
        );
    }

    let data = match collect_current_context_candidates(repl, query_str).await {
        Some(d) => d,
        None => return CompletionSelection::None,
    };

    if data.items.is_empty() {
        return CompletionSelection::None;
    }

    // Cache the result
    // We only cache if we have results, to avoid caching "not found" states too aggressively
    // if the user is typing quickly.
    // However, caching partial results might be good.
    // For now, let's cache what we found.
    LEGACY_COMPLETION_CACHE.set(query_str.to_string(), data.items.clone());

    select_completion_items(
        data.items,
        data.selection_query.as_deref(),
        prompt_text,
        input_text,
        framework,
    )
}

#[derive(Debug, Clone)]
struct CurrentCompletionData {
    items: Vec<Candidate>,
    selection_query: Option<String>,
}

async fn collect_current_context_candidates(
    repl: &Repl<'_>,
    query_str: &str,
) -> Option<CurrentCompletionData> {
    let lisp_engine = Rc::clone(&repl.shell.lisp_engine);
    let environment = Arc::clone(&lisp_engine.borrow().shell_env);

    // Determine current directory for relative path resolution
    let current_dir = std::env::current_dir().unwrap_or_else(|e| {
        warn!(
            "Failed to get current directory: {}, using home directory",
            e
        );
        std::env::var("HOME")
            .map(std::path::PathBuf::from)
            .ok()
            .unwrap_or_else(|| {
                warn!("Failed to get home directory, using root");
                std::path::PathBuf::from("/")
            })
    });

    // Expand tilde (~) in the query path
    let expanded = shellexpand::tilde(query_str).to_string();
    let expanded_path = Path::new(&expanded);

    // Determine the path to search and query substring
    let (search_path, path_query, only_path) = if expanded_path.is_dir() {
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
        (current_dir.clone(), query_str.to_string(), false)
    };

    // Canonicalize the path for consistent resolution
    let canonical_path = search_path
        .canonicalize()
        .unwrap_or_else(|_| current_dir.clone());
    let canonical_str = canonical_path.display().to_string();
    let search_path_str = search_path.to_str()?.to_string();

    let mut items = if path_query.is_empty() {
        get_file_completions(&canonical_str, &search_path_str).await
    } else {
        get_file_completions_with_filter(&canonical_str, &search_path_str, Some(&path_query)).await
    };

    if !only_path {
        // Paths cloning for thread safety
        let paths = environment.read().paths.clone();
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
mod tests {
    use super::*;

    #[test]
    fn default_framework_switches_to_skim_via_env() {
        let original = std::env::var("DSH_COMPLETION_FRAMEWORK").ok();
        unsafe {
            std::env::set_var("DSH_COMPLETION_FRAMEWORK", "skim");
        }
        assert_eq!(
            super::default_completion_framework(),
            CompletionFrameworkKind::Skim
        );
        match original {
            Some(value) => unsafe {
                std::env::set_var("DSH_COMPLETION_FRAMEWORK", value);
            },
            None => unsafe {
                std::env::remove_var("DSH_COMPLETION_FRAMEWORK");
            },
        }
    }

    #[test]
    fn test_fuzzy_file_completion_with_temp_dir() {
        use std::fs::File;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let dir_path = dir.path().to_str().unwrap();

        // Create some files
        File::create(dir.path().join("completion.rs")).unwrap();
        File::create(dir.path().join("command.rs")).unwrap();
        File::create(dir.path().join("cache.rs")).unwrap();
        File::create(dir.path().join("notes.txt")).unwrap();

        // Test fuzzy match "cmp" -> should find completion.rs
        let results = super::get_file_completions_with_filter_sync(dir_path, "", Some("cmp"));
        assert!(!results.is_empty());
        assert!(
            results
                .iter()
                .any(|c| matches!(c, Candidate::Item(name, _) if name.contains("completion.rs")))
        );

        // Test fuzzy match "cc" -> should find cache.rs (and maybe command.rs / completion.rs depending on score, but definitely cache.rs)
        let results_cc = super::get_file_completions_with_filter_sync(dir_path, "", Some("cc"));
        assert!(!results_cc.is_empty());
        assert!(
            results_cc
                .iter()
                .any(|c| matches!(c, Candidate::Item(name, _) if name.contains("cache.rs")))
        );

        // Test no match
        let results_none = super::get_file_completions_with_filter_sync(dir_path, "", Some("xyz"));
        assert!(results_none.is_empty());
    }

    #[test]
    fn test_last_word_basic() {
        assert_eq!(last_word("git status"), "status");
        assert_eq!(last_word("git"), "git");
        assert_eq!(last_word("a b c d"), "d");
    }

    #[test]
    fn test_last_word_whitespace_only() {
        assert_eq!(last_word("   "), "");
    }

    #[test]
    fn test_last_word_empty() {
        assert_eq!(last_word(""), "");
    }

    #[test]
    fn test_get_prompt_and_input_defaults() {
        let (prompt, input) = get_prompt_and_input_for_completion();
        assert_eq!(prompt, "$ ");
        assert_eq!(input, "");
    }

    #[test]
    fn test_select_completion_items_empty_returns_none() {
        let result =
            select_completion_items(vec![], None, "$ ", "", CompletionFrameworkKind::Inline);
        assert!(matches!(result, CompletionSelection::None));
    }

    #[test]
    fn test_select_completion_items_single_fast_path() {
        let items = vec![Candidate::Basic("hello".to_string())];
        let result =
            select_completion_items(items, None, "$ ", "h", CompletionFrameworkKind::Inline);
        // Single candidate → fast path returns Selected
        assert!(matches!(result, CompletionSelection::Selected(ref s) if s == "hello"));
    }

    #[test]
    fn test_get_file_completions_nonexistent_dir() {
        let results = get_file_completions_with_filter_sync("/nonexistent/dir/12345", "", None);
        assert!(results.is_empty());
    }

    #[test]
    fn test_get_file_completions_directories_labeled() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let dir_path = dir.path().to_str().unwrap();

        // Create a subdirectory
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        // Create a file
        std::fs::File::create(dir.path().join("file.txt")).unwrap();

        let results = get_file_completions_with_filter_sync(dir_path, "", None);
        assert_eq!(results.len(), 2);

        // Check labeling
        let has_dir_label = results
            .iter()
            .any(|c| matches!(c, Candidate::Item(_, desc) if desc == "(directory)"));
        let has_file_label = results
            .iter()
            .any(|c| matches!(c, Candidate::Item(_, desc) if desc == "(file)"));
        assert!(has_dir_label, "Directory should be labeled (directory)");
        assert!(has_file_label, "File should be labeled (file)");
    }
}
