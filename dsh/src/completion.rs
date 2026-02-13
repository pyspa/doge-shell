use self::cache::CompletionCache;
use self::framework::{CompletionFrameworkKind, CompletionRequest, select_with_framework_kind};
use crate::input::Input;
use crate::lisp::Value;
use crate::repl::Repl;
use dsh_frecency::{ItemStats, SortMethod};
use skim::SkimItem;
use std::collections::BTreeSet;
use std::fs::read_dir;
use std::path::Path;
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::Duration;
use std::{process::Command, sync::Arc};
use tracing::debug;
use tracing::warn;

pub mod cache;
pub mod command;
pub mod commands;
pub mod context;
pub mod display;
pub mod fuzzy;
pub mod path;
pub mod skim_adapter;

pub mod errors;
pub mod framework;
pub mod generator;
pub mod generators;
mod history;
pub mod integrated;
pub mod json_loader;
pub mod parser;
mod ui;

#[cfg(test)]
mod extra_tests;
#[cfg(test)]
mod wrapped_tests;
#[cfg(test)]
mod z_tests;

// Re-export from completion module
pub use crate::completion::command::CompletionType;
pub use crate::completion::commands::{deduplicate_candidates, get_commands};
pub use crate::completion::display::Candidate;
pub use crate::completion::display::CompletionConfig;
pub use crate::completion::framework::CompletionSelection;
pub use crate::completion::fuzzy::fuzzy_match_score;
pub use crate::completion::path::*;
pub use crate::completion::skim_adapter::{replace_space, select_item_with_skim};

pub const MAX_RESULT: usize = 500;

const LEGACY_CACHE_TTL_MS: u64 = 3000;

static LEGACY_COMPLETION_CACHE: LazyLock<CompletionCache<Candidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(LEGACY_CACHE_TTL_MS)));

#[derive(Debug, Clone)]
pub struct AutoComplete {
    pub target: String,
    pub cmd: Option<String>,
    pub func: Option<Value>,
    pub candidates: Option<Vec<String>>,
}

use std::sync::OnceLock;
use tokio::sync::mpsc::UnboundedSender;

static COMPLETION_NOTIFIER: OnceLock<UnboundedSender<()>> = OnceLock::new();

pub fn set_completion_notifier(sender: UnboundedSender<()>) {
    let _ = COMPLETION_NOTIFIER.set(sender);
}

pub fn notify_completion_update() {
    if let Some(sender) = COMPLETION_NOTIFIER.get() {
        // UnboundedSender has send method which is non-blocking/synchronous
        let _ = sender.send(());
    }
}

/// Main completion structure
#[derive(Debug)]
pub struct Completion {
    pub input: Option<String>,
    pub current_index: usize,
    pub completions: Vec<ItemStats>,
}

impl Default for Completion {
    fn default() -> Self {
        Self::new()
    }
}

impl Completion {
    pub fn new() -> Self {
        Completion {
            input: None,
            current_index: 0,
            completions: Vec::new(),
        }
    }

    pub fn is_changed(&self, word: &str) -> bool {
        if let Some(input) = &self.input {
            input != word
        } else {
            !word.is_empty()
        }
    }

    pub fn clear(&mut self) {
        self.input = None;
        self.current_index = 0;
        self.completions = Vec::new();
    }

    pub fn completion_mode(&self) -> bool {
        !self.completions.is_empty()
    }

    pub fn set_completions(&mut self, input: &str, comps: Vec<ItemStats>) {
        let item = ItemStats::new(input, 0.0, 1.0, None);

        self.input = if input.is_empty() {
            None
        } else {
            Some(input.to_string())
        };
        self.completions = comps;
        self.completions.insert(0, item);
        self.current_index = 0;
    }

    pub fn backward(&mut self) -> Option<&ItemStats> {
        if self.completions.is_empty() {
            return None;
        }

        if self.completions.len() - 1 > self.current_index {
            self.current_index += 1;
            Some(&self.completions[self.current_index])
        } else {
            None
        }
    }

    pub fn forward(&mut self) -> Option<&ItemStats> {
        if self.current_index > 0 {
            self.current_index -= 1;
            Some(&self.completions[self.current_index])
        } else {
            None
        }
    }
}

// Helper function to get current prompt and input for completion display
pub(crate) fn get_prompt_and_input_for_completion() -> (String, String) {
    // For backward compatibility, return reasonable defaults
    // In practice, the main completion path should use the version with explicit parameters
    ("$ ".to_string(), "".to_string())
}

pub(crate) fn last_word(s: &str) -> &str {
    s.split_whitespace().last().unwrap_or("")
}

pub(super) fn default_completion_framework() -> CompletionFrameworkKind {
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
) -> CompletionSelection {
    select_completion_items_with_config(
        items,
        query,
        prompt_text,
        input_text,
        CompletionConfig::default(),
    )
}

pub fn select_completion_items_with_config(
    items: Vec<Candidate>,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    config: CompletionConfig,
) -> CompletionSelection {
    select_completion_items_with_framework(
        items,
        query,
        prompt_text,
        input_text,
        config,
        default_completion_framework(),
    )
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
pub fn select_completion_items_simple(
    items: Vec<Candidate>,
    query: Option<&str>,
) -> CompletionSelection {
    let (prompt_text, input_text) = get_prompt_and_input_for_completion();
    select_completion_items_with_framework(
        items,
        query,
        &prompt_text,
        &input_text,
        CompletionConfig::default(),
        CompletionFrameworkKind::Inline,
    )
}

pub fn completion_from_cmd(input: String, query: Option<&str>) -> CompletionSelection {
    debug!("{} ", &input);
    match Command::new("sh").arg("-c").arg(input).output() {
        Ok(output) => {
            if let Ok(out) = String::from_utf8(output.stdout) {
                let items: Vec<Candidate> = out
                    .lines()
                    .map(|x| x.trim())
                    .filter(|x| !x.is_empty())
                    .map(|x| Candidate::Basic(x.to_string()))
                    .collect();

                return select_completion_items_simple(items, query);
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
    // Main fallback completion function that tries multiple completion sources in sequence:
    // 1. Lisp-based completion (custom completion definitions)
    // 2. Current context completion (path completion, command completion from PATH)
    // 3. ChatGPT completion (if enabled and API key is set)

    debug!("input_completion starting with query: {:?}", query);

    // Try lisp-based completion first (custom completions defined by user)
    // Lisp logic is synchronous but fast (unless user func is slow).
    // We keep it synchronous for now or wrap it if needed.
    let res = completion_from_lisp_with_prompt(input, repl, query, prompt_text, input_text);
    if let CompletionSelection::Selected(_) | CompletionSelection::Interactive(..) = res {
        debug!("Lisp completion returned result: {:?}", res);
        return res;
    }

    // Try z completion
    if let CompletionSelection::Selected(_) | CompletionSelection::Interactive(..) =
        completion_for_z(input, repl, query, prompt_text, input_text)
    {
        return completion_for_z(input, repl, query, prompt_text, input_text);
    }

    // Try current context completion (files, directories, commands in PATH)
    let res =
        completion_from_current_with_prompt(input, repl, query, prompt_text, input_text).await;
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
                        return select_completion_items(items, query, prompt_text, input_text);
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
                let res = completion_from_cmd(cmd.to_string(), query);
                if let CompletionSelection::Selected(val) = res {
                    if val.starts_with('*') {
                        return CompletionSelection::Selected(val[2..].to_string());
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
                return select_completion_items(items, query, prompt_text, input_text);
            }
            return CompletionSelection::None;
        }
    }
    CompletionSelection::None
}

fn completion_for_z(
    input: &Input,
    repl: &Repl,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
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

        return select_completion_items(candidates, query, prompt_text, input_text);
    }
    CompletionSelection::None
}

async fn completion_from_current_with_prompt(
    _input: &Input,
    repl: &Repl<'_>,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
) -> CompletionSelection {
    debug!("completion_from_current_with_prompt: query={:?}", query);

    let query_str = if let Some(q) = query.filter(|q| !q.is_empty()) {
        q
    } else {
        return CompletionSelection::None;
    };

    debug!("cache query_str: '{}'", query_str);
    if let Some(hit) = LEGACY_COMPLETION_CACHE.lookup(query_str) {
        debug!(
            "cache hit for query '{}' (key: '{}', len: {})",
            query_str,
            hit.key,
            hit.candidates.len()
        );
        if hit.exact || !hit.candidates.is_empty() {
            LEGACY_COMPLETION_CACHE.extend_ttl(&hit.key);
            return select_completion_items(
                hit.candidates,
                Some(query_str),
                prompt_text,
                input_text,
            );
        }
    }

    let data = match collect_current_context_candidates(repl, query_str).await {
        Some(d) => d,
        None => return CompletionSelection::None,
    };

    if data.items.is_empty() {
        return CompletionSelection::None;
    }

    LEGACY_COMPLETION_CACHE.set(query_str.to_string(), data.items.clone());

    select_completion_items(
        data.items,
        data.selection_query.as_deref(),
        prompt_text,
        input_text,
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

fn get_file_completions_with_filter_sync(
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

/// Legacy compatibility functions
/// These functions provide backward compatibility with the existing codebase
#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

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
    fn test_fuzzy_match_score() {
        // Exact match
        let score_exact = super::fuzzy_match_score("test", "test").unwrap();
        assert!(score_exact > 0);

        // Prefix match
        let score_prefix = super::fuzzy_match_score("terminal", "term").unwrap();
        assert!(score_prefix > 0);

        // Fuzzy match
        let score_fuzzy = super::fuzzy_match_score("terminal", "trm").unwrap();
        assert!(score_fuzzy > 0);

        // No match
        let score_none = super::fuzzy_match_score("terminal", "xyz");
        assert!(score_none.is_none());

        // Match comparison
        // "src/completion.rs" should match "cmp"
        let score_cmp = super::fuzzy_match_score("src/completion.rs", "cmp").unwrap();
        assert!(score_cmp > 0);
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
    fn test_path_completion_prefix_fuzzy() {
        use std::fs::File;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let dir_path = dir.path();

        // Create files: apple.txt, application.rs, banana.md
        File::create(dir_path.join("apple.txt")).unwrap();
        File::create(dir_path.join("application.rs")).unwrap();
        File::create(dir_path.join("banana.md")).unwrap();

        // Construct paths for input
        let dir_str = dir_path.to_str().unwrap();

        // Case 1: "apl" -> "apple.txt"
        let input_apl = format!("{}/apl", dir_str);
        let result_apl = super::path_completion_prefix(&input_apl).unwrap();

        assert!(result_apl.is_some());
        let val = result_apl.unwrap();
        assert!(
            val.ends_with("apple.txt"),
            "Expected apple.txt, got {}",
            val
        );

        // Case 2: "bn" -> "banana.md"
        let input_bn = format!("{}/bn", dir_str);
        let result_bn = super::path_completion_prefix(&input_bn).unwrap();
        assert!(result_bn.is_some());
        assert!(result_bn.unwrap().ends_with("banana.md"));

        // Case 3: "z" -> no match
        let input_z = format!("{}/z", dir_str);
        let result_z = super::path_completion_prefix(&input_z).unwrap();
        assert!(result_z.is_none());
    }

    #[test]
    fn test_path_completion_relative() {
        use std::fs::File;
        use tempfile::tempdir;

        // This test simulates path_completion_prefix interacting with "./" paths
        // We can't easily mock "full" path completion context without changing current dir
        // But we can test ranking logic if we had relative paths.

        let dir = tempdir().unwrap();
        let dir_path = dir.path();
        File::create(dir_path.join("README.md")).unwrap();

        let dir_str = dir_path.to_str().unwrap();

        // Input: "R" -> "README.md"
        // But input usually needs to be relative or absolute path.
        // path_completion_prefix logic:
        // if input is just "R" -> parent is None/"" -> uses current dir.
        // We can't change current dir easily in multi-threaded test environment safely
        // without affecting other tests.
        // So we test absolute path behavior which we can control.

        let input = format!("{}/R", dir_str);
        let result = super::path_completion_prefix(&input).unwrap();
        assert!(result.unwrap().ends_with("README.md"));

        // Test fuzzy: "RME" -> "README.md"
        let input_fuzzy = format!("{}/RME", dir_str);
        let result_fuzzy = super::path_completion_prefix(&input_fuzzy).unwrap();
        assert!(result_fuzzy.unwrap().ends_with("README.md"));
    }

    #[test]
    fn test_fuzzy_match_ranking() {
        // "test" matches "test" (score X)
        // "test" matches "test_file" (score Y)
        // Exact match should have higher score or be preferred?
        // Let's verify score property.

        let score_exact = super::fuzzy_match_score("test", "test").unwrap();
        let score_prefix = super::fuzzy_match_score("test_file", "test").unwrap();
        let score_fuzzy = super::fuzzy_match_score("t_e_s_t", "test").unwrap();

        // Exact match usually scores highest in Skim
        assert!(score_exact >= score_prefix);
        assert!(score_prefix > score_fuzzy);
    }
}
