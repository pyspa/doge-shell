use self::cache::CompletionCache;
use self::framework::{CompletionFrameworkKind, CompletionRequest, select_with_framework_kind};
use crate::dirs::is_executable;
use crate::input::Input;
use crate::lisp::Value;
use crate::repl::Repl;
use anyhow::Result;
use dsh_frecency::ItemStats;
use regex::Regex;
use skim::prelude::*;
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fs::read_dir;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::Duration;
use std::{process::Command, sync::Arc};
use tracing::debug;
use tracing::warn;

mod cache;
mod command;
pub mod display;

pub mod framework;
mod generator;
pub mod generators;
mod history;
pub mod integrated;
pub mod json_loader;
pub mod parser;
mod ui;

#[cfg(test)]
mod extra_tests;

// Re-export from completion module
pub use crate::completion::command::CompletionType;
pub use crate::completion::display::Candidate;
pub use crate::completion::display::CompletionConfig;

pub const MAX_RESULT: usize = 500;

const LEGACY_CACHE_TTL_MS: u64 = 3000;

static LEGACY_COMPLETION_CACHE: LazyLock<CompletionCache<Candidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(LEGACY_CACHE_TTL_MS)));

static PATH_COMPLETION_CACHE: LazyLock<CompletionCache<Candidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(2000)));

#[derive(Debug, Clone)]
pub struct AutoComplete {
    pub target: String,
    pub cmd: Option<String>,
    pub func: Option<Value>,
    pub candidates: Option<Vec<String>>,
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
        let item = ItemStats::new(input, 0.0, 1.0);

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

pub fn path_completion_prefix(input: &str) -> Result<Option<String>> {
    let pbuf = PathBuf::from(input);
    let absolute = pbuf.is_absolute();
    let file_name = pbuf.file_name();
    if file_name.is_none() {
        return Ok(None);
    }
    let parent = pbuf.parent();
    let search = input.to_string();

    let paths = if absolute {
        let dir = if let Some(f) = parent {
            f.to_string_lossy().to_string()
        } else {
            input.to_string()
        };
        path_completion_path(PathBuf::from(dir))?
    } else if let Some(dir) = parent {
        if dir.display().to_string().is_empty() {
            // current dir
            path_completion_path(PathBuf::from("."))?
        } else {
            path_completion_path(PathBuf::from(dir))?
        }
    } else {
        path_completion()?
    };

    for cand in paths.iter() {
        if let Candidate::Path(path) = cand {
            let path_str = path.to_string();
            if path.starts_with(&search) {
                return Ok(Some(path_str));
            }

            if let Ok(striped) = PathBuf::from(path).strip_prefix("./") {
                let striped_str = striped.display().to_string();
                if striped_str.starts_with(&search) {
                    return Ok(Some(path_str[2..].to_string()));
                }
            }
        }
    }
    Ok(None)
}

fn path_is_dir(path: &PathBuf) -> Result<bool> {
    if let Ok(mut metadata) = path.metadata() {
        if metadata.is_symlink() {
            let link = std::fs::read_link(path)?;
            let relative = link.is_relative();
            if relative {
                metadata = path.join(&link).metadata()?;
            }
        }
        Ok(metadata.is_dir())
    } else {
        Ok(false)
    }
}

pub fn path_completion() -> Result<Vec<Candidate>> {
    let current_dir = std::env::current_dir()?;
    path_completion_path(current_dir)
}

pub fn path_completion_path(path: PathBuf) -> Result<Vec<Candidate>> {
    let path_str = path.display().to_string();

    // Check cache first
    if let Some(hit) = PATH_COMPLETION_CACHE.lookup(&path_str) {
        // We accept empty results from cache too if that directory is truly empty
        // But invalid/expired cache is handled by lookup returning None
        return Ok(hit.candidates);
    }

    let exp_str = shellexpand::tilde(&path_str).to_string();
    let expand = path_str != exp_str;

    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .ok()
        .ok_or_else(|| anyhow::Error::msg("HOME environment variable not set"))?;
    let path = PathBuf::from(exp_str);

    let dir = read_dir(&path)?;
    let mut files: Vec<Candidate> = Vec::new();

    for entry in dir.flatten() {
        let entry_path = entry.path();
        let is_dir = path_is_dir(&entry_path)?;
        if expand {
            if let Ok(part) = entry_path.strip_prefix(&home) {
                let mut pb = PathBuf::new();
                pb.push("~/");
                pb.push(part);
                let mut path = pb.display().to_string();
                if is_dir {
                    path += "/";
                }
                files.push(Candidate::Path(path));
            }
        } else {
            let mut path = entry_path.display().to_string();
            if is_dir {
                path += "/";
            }
            files.push(Candidate::Path(path));
        }
    }
    files.sort();

    // Update cache
    PATH_COMPLETION_CACHE.set(path_str, files.clone());

    Ok(files)
}

impl SkimItem for Candidate {
    fn output(&self) -> Cow<'_, str> {
        match self {
            Candidate::Item(x, _) => Cow::Borrowed(x),
            Candidate::Path(p) => Cow::Borrowed(p),
            Candidate::Basic(x) => Cow::Borrowed(x),
            Candidate::Command { name, .. } => Cow::Borrowed(name),
            Candidate::Option { name, .. } => Cow::Borrowed(name),
            Candidate::File { path, .. } => Cow::Borrowed(path),
            Candidate::GitBranch { name, .. } => Cow::Borrowed(name),
            Candidate::History { command, .. } => Cow::Borrowed(command),
        }
    }

    fn text(&self) -> Cow<'_, str> {
        match self {
            Candidate::Item(x, y) => {
                let desc = format!("{x:<30} {y}");
                Cow::Owned(desc)
            }
            Candidate::Path(p) => Cow::Borrowed(p),
            Candidate::Basic(x) => Cow::Borrowed(x),
            Candidate::Command { name, description } => {
                let icon = "⚡"; // Command icon
                if description.is_empty() {
                    Cow::Owned(format!("{icon} {name}"))
                } else {
                    Cow::Owned(format!("{icon} {name:<30} {description}"))
                }
            }
            Candidate::Option { name, description } => {
                let icon = "🔧"; // Option icon
                if description.is_empty() {
                    Cow::Owned(format!("{icon} {name}"))
                } else {
                    Cow::Owned(format!("{icon} {name:<30} {description}"))
                }
            }
            Candidate::File { path, is_dir } => {
                let type_indicator = if *is_dir { "/" } else { "" };
                Cow::Owned(format!("{path}{type_indicator}"))
            }
            Candidate::GitBranch { name, is_current } => {
                let indicator = if *is_current { " (current)" } else { "" };
                Cow::Owned(format!("{name}{indicator}"))
            }
            Candidate::History {
                command, frequency, ..
            } => {
                let desc = format!("{command:<30} used {frequency} times");
                Cow::Owned(desc)
            }
        }
    }
}

pub fn select_item_with_skim(items: Vec<Candidate>, query: Option<&str>) -> Option<String> {
    let (prompt_text, input_text) = get_prompt_and_input_for_completion();
    select_completion_items_with_framework(
        items,
        query,
        &prompt_text,
        &input_text,
        CompletionConfig::default(),
        CompletionFrameworkKind::Skim,
    )
}

// Helper function to get current prompt and input for completion display
fn get_prompt_and_input_for_completion() -> (String, String) {
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
) -> Option<String> {
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
) -> Option<String> {
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
) -> Option<String> {
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
        return None;
    }

    let request = CompletionRequest::new(items, query, prompt_text, input_text, config);
    select_with_framework_kind(framework, request)
}

// Backward compatibility function
pub fn select_completion_items_simple(
    items: Vec<Candidate>,
    query: Option<&str>,
) -> Option<String> {
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

pub fn completion_from_cmd(input: String, query: Option<&str>) -> Option<String> {
    debug!("{} ", &input);
    match Command::new("sh").arg("-c").arg(input).output() {
        Ok(output) => {
            if let Ok(out) = String::from_utf8(output.stdout) {
                let items: Vec<Candidate> = out
                    .split('\n')
                    // TODO filter
                    .map(|x| Candidate::Basic(x.trim().to_string()))
                    .collect();

                return select_completion_items_simple(items, query);
            }
            None
        }
        _ => None,
    }
}

pub fn input_completion(
    input: &Input,
    repl: &Repl,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
) -> Option<String> {
    // Main fallback completion function that tries multiple completion sources in sequence:
    // 1. Lisp-based completion (custom completion definitions)
    // 2. Current context completion (path completion, command completion from PATH)
    // 3. ChatGPT completion (if enabled and API key is set)

    debug!("input_completion starting with query: {:?}", query);

    // Try lisp-based completion first (custom completions defined by user)
    let res = completion_from_lisp_with_prompt(input, repl, query, prompt_text, input_text);
    if res.is_some() {
        debug!("Lisp completion returned result: {:?}", res);
        return res;
    }

    // Try current context completion (files, directories, commands in PATH)
    let res = completion_from_current_with_prompt(input, repl, query, prompt_text, input_text);
    if res.is_some() {
        debug!("Context completion returned result: {:?}", res);
        return res;
    }

    debug!("No completion candidates found from any source");

    // Return None if no completion sources provided any candidates
    // This is the "silent failure" behavior when no matches are found from any source
    // No error message is shown to user, maintaining the "no visible effect" behavior
    None
}

fn completion_from_lisp_with_prompt(
    input: &Input,
    repl: &Repl,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
) -> Option<String> {
    // TODO convert input
    let lisp_engine = Rc::clone(&repl.shell.lisp_engine);
    let environment = Arc::clone(&lisp_engine.borrow().shell_env);

    // 1. completion from autocomplete
    for compl in environment.read().autocompletion.iter() {
        let cmd_str = compl.target.to_string();
        // debug!("match cmd:'{}' in:'{}'", cmd_str, replace_space(input));
        if replace_space(input.as_str()).starts_with(cmd_str.as_str()) {
            if let Some(func) = &compl.func {
                // run lisp func
                match lisp_engine.borrow().apply_func(func.to_owned(), vec![]) {
                    Ok(Value::List(list)) => {
                        let mut items: Vec<Candidate> = Vec::new();
                        for val in list.into_iter() {
                            items.push(Candidate::Basic(val.to_string()));
                        }
                        return select_completion_items(items, query, prompt_text, input_text);
                    }
                    Ok(Value::String(str)) => {
                        return Some(str);
                    }
                    Err(err) => {
                        eprintln!("{err:?}");
                    }
                    _ => {}
                }
            } else if let Some(cmd) = &compl.cmd {
                // run command
                if let Some(val) = completion_from_cmd(cmd.to_string(), query) {
                    if val.starts_with('*') {
                        return Some(val[2..].to_string());
                    } else {
                        return Some(val);
                    }
                }
            } else if let Some(items) = &compl.candidates {
                let items: Vec<Candidate> = items
                    .iter()
                    .map(|x| Candidate::Basic(x.trim().to_string()))
                    .collect();
                return select_completion_items(items, query, prompt_text, input_text);
            }
            return None;
        }
    }
    None
}

fn completion_from_current_with_prompt(
    _input: &Input,
    repl: &Repl,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
) -> Option<String> {
    debug!("completion_from_current_with_prompt: query={:?}", query);

    let query_str = query.filter(|q| !q.is_empty())?;

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

    let data = collect_current_context_candidates(repl, query_str)?;

    if data.items.is_empty() {
        return None;
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

fn collect_current_context_candidates(
    repl: &Repl,
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
    let search_path_str = search_path.to_str()?;

    let mut items = if path_query.is_empty() {
        get_file_completions(canonical_str.as_str(), search_path_str)
    } else {
        get_file_completions_with_filter(
            canonical_str.as_str(),
            search_path_str,
            Some(path_query.as_str()),
        )
    };

    if !only_path {
        let mut command_items = get_commands(&environment.read().paths, query_str);
        items.append(&mut command_items);
        items = deduplicate_candidates(items);
    }

    Some(CurrentCompletionData {
        items,
        selection_query: Some(path_query),
    })
}

fn get_commands(paths: &Vec<String>, cmd: &str) -> Vec<Candidate> {
    let mut list = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    if cmd.starts_with('/') {
        let cmd_path = std::path::Path::new(cmd);
        if cmd_path.exists() && cmd_path.is_file() {
            // Extract filename for deduplication? Or just add?
            // Absolute paths usually don't need dedupe against PATH commands by name unless we want to?
            // Current logic treated them as "(command)".
            // Let's keep it simple.
            list.push(Candidate::Item(cmd.to_string(), "(command)".to_string()));
        }
    }
    if cmd.starts_with("./") {
        let cmd_path = std::path::Path::new(cmd);
        if cmd_path.exists() && cmd_path.is_file() {
            list.push(Candidate::Item(cmd.to_string(), "(command)".to_string()));
        }
    }

    for path in paths {
        get_executables_into(path, cmd, &mut list, &mut seen_names);
    }

    // No need to call deduplicate_candidates(list) here if we trust our seen_names logic for commands.
    // However, deduplicate_candidates also handles file vs command priority.
    // But get_commands ONLY produces commands.
    // So we are safe.
    list
}

fn get_executables_into(
    dir: &str,
    name: &str,
    list: &mut Vec<Candidate>,
    seen: &mut std::collections::HashSet<String>,
) {
    match read_dir(dir) {
        Ok(entries) => {
            // Optimization: Filter entries while iterating to avoid collecting all files in PATH
            // (which can be thousands) before sorting.
            // We can't easily sort if we stream directly to list.
            // But sorting per-directory is less important than sorting the final result?
            // Actually, we usually want the final list sorted.
            // Existing logic sorted `candidates` from ONE directory.
            // If we append unsorted, the final list might be unsorted.
            // `select_completion_items` usually handles sorting via Skim or we might expect sorted input.
            // Skim sorts. Inline completion might expect sorted?
            // The existing `get_executables` returns a sorted list.
            // But `get_commands` appends them in PATH order.
            // So `list` in `get_commands` is blocked by PATH order (bin matches, then usr/bin matches).
            // Within `bin`, they are sorted.

            // Let's collect local candidates, sort them, then push unique ones to global list.

            let mut local_candidates: Vec<String> = Vec::new();

            for entry in entries.flatten() {
                let file_name_os = entry.file_name();
                let Some(file_name) = file_name_os.to_str() else {
                    continue;
                };

                if !file_name.starts_with(name) {
                    continue;
                }

                // Check seen
                if seen.contains(file_name) {
                    continue;
                }

                // Optimization: check file type from entry if possible
                if let Ok(ft) = entry.file_type()
                    && !ft.is_file()
                    && !ft.is_symlink()
                {
                    continue;
                }

                if is_executable(&entry) {
                    local_candidates.push(file_name.to_string());
                }
            }

            local_candidates.sort();

            for candle in local_candidates {
                // Double check seen (though we checked before, but sorting prevents race? no race, it's serial)
                // We checked before `is_executable`.
                if seen.insert(candle.clone()) {
                    list.push(Candidate::Item(candle, "(command)".to_string()));
                }
            }
        }
        Err(_err) => {}
    }
}

// Keeping signature for compatibility if used elsewhere (it's not pub but module-local)
// Actually it is not pub.
#[allow(dead_code)]
fn get_executables(dir: &str, name: &str) -> Vec<Candidate> {
    let mut list = Vec::new();
    let mut seen = std::collections::HashSet::new();
    get_executables_into(dir, name, &mut list, &mut seen);
    list
}

/// Deduplicate candidates, prioritizing commands over files for the same name
fn deduplicate_candidates(items: Vec<Candidate>) -> Vec<Candidate> {
    debug!("deduplicate_candidates: input items count={}", items.len());
    let mut seen_names = std::collections::HashMap::new();
    let mut result = Vec::new();

    for candidate in items {
        let (name, _description) = match &candidate {
            Candidate::Item(name, desc) => (name.clone(), desc.clone()),
            Candidate::Path(name) => (name.clone(), "(path)".to_string()),
            Candidate::Basic(name) => (name.clone(), "(basic)".to_string()),
            Candidate::Command { name, description } => (name.clone(), description.clone()),
            Candidate::Option { name, description } => (name.clone(), description.clone()),
            Candidate::GitBranch { name, .. } => (name.clone(), "(git-branch)".to_string()),
            Candidate::File { path, is_dir } => (
                path.clone(),
                if *is_dir { "(directory)" } else { "(file)" }.to_string(),
            ),
            Candidate::History { command, .. } => (command.clone(), "(history)".to_string()),
        };

        // Extract just the filename for comparison (remove path prefixes)
        let base_name = if let Some(pos) = name.rfind('/') {
            &name[pos + 1..]
        } else {
            &name
        };

        match seen_names.get(base_name) {
            Some(existing_idx) => {
                // debug!(
                //     "deduplicate_candidates: found duplicate base_name='{}', name='{}'",
                //     base_name, name
                // );
                // If we already have this name, prioritize commands over files
                let existing_candidate = &result[*existing_idx];
                let should_replace = match (&existing_candidate, &candidate) {
                    // Replace file with command
                    (Candidate::Item(_, existing_desc), Candidate::Item(_, new_desc))
                        if existing_desc == "(file)" && new_desc == "(command)" =>
                    {
                        debug!(
                            "deduplicate_candidates: replacing file with command for '{}'",
                            base_name
                        );
                        true
                    }
                    // Don't replace command with file
                    (Candidate::Item(_, existing_desc), Candidate::Item(_, new_desc))
                        if existing_desc == "(command)" && new_desc == "(file)" =>
                    {
                        debug!(
                            "deduplicate_candidates: keeping command over file for '{}'",
                            base_name
                        );
                        false
                    }
                    // For other cases, keep the first one
                    _ => {
                        debug!(
                            "deduplicate_candidates: keeping first occurrence for '{}'",
                            base_name
                        );
                        false
                    }
                };

                if should_replace {
                    result[*existing_idx] = candidate;
                }
            }
            None => {
                // First time seeing this name
                // debug!(
                //     "deduplicate_candidates: adding new candidate base_name='{}', name='{}'",
                //     base_name, name
                // );
                seen_names.insert(base_name.to_string(), result.len());
                result.push(candidate);
            }
        }
    }

    debug!(
        "deduplicate_candidates: output items count={}",
        result.len()
    );
    result
}

fn get_file_completions(dir: &str, prefix: &str) -> Vec<Candidate> {
    debug!("get_file_completions: dir={}, prefix={}", dir, prefix);
    get_file_completions_with_filter(dir, prefix, None)
}

fn get_file_completions_with_filter(
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
                    && !file_name.starts_with(filter)
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

// Pre-compiled regex for whitespace replacement - compiled once at first use
static WHITESPACE_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\s+").unwrap());

fn replace_space(s: &str) -> String {
    WHITESPACE_REGEX.replace_all(s, "_").to_string()
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
    fn test_deduplicate_candidates() {
        // Test deduplication with command priority over file
        let items = vec![
            Candidate::Item("test".to_string(), "(file)".to_string()),
            Candidate::Item("test".to_string(), "(command)".to_string()),
            Candidate::Item("other".to_string(), "(file)".to_string()),
        ];

        let result = deduplicate_candidates(items);

        assert_eq!(result.len(), 2);
        // Should keep command version of "test", not file version
        assert!(result.iter().any(
            |c| matches!(c, Candidate::Item(name, desc) if name == "test" && desc == "(command)")
        ));
        assert!(result.iter().any(
            |c| matches!(c, Candidate::Item(name, desc) if name == "other" && desc == "(file)")
        ));
        // Should not have file version of "test"
        assert!(!result.iter().any(
            |c| matches!(c, Candidate::Item(name, desc) if name == "test" && desc == "(file)")
        ));
    }

    #[test]
    fn test_deduplicate_candidates_with_paths() {
        // Test deduplication with path prefixes
        let items = vec![
            Candidate::Item("/usr/bin/ls".to_string(), "(command)".to_string()),
            Candidate::Item("./ls".to_string(), "(file)".to_string()),
            Candidate::Item("ls".to_string(), "(command)".to_string()),
        ];

        let result = deduplicate_candidates(items);

        // Should deduplicate based on base filename "ls"
        assert_eq!(result.len(), 1);
        // Should keep the first command version
        assert!(result.iter().any(|c| matches!(c, Candidate::Item(name, desc) if name == "/usr/bin/ls" && desc == "(command)")));
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
    fn test_select_item_with_skim_single_candidate() {
        // Test that single candidate is returned directly without UI
        let single_candidate = vec![Candidate::Basic("single_item".to_string())];
        let result = select_item_with_skim(single_candidate, None);
        assert_eq!(result, Some("single_item".to_string()));
    }

    #[test]
    #[ignore] // Ignored because it requires user interaction
    fn test_select_item_with_skim_multiple_candidates() {
        // Test that multiple candidates still require UI selection (would return None in test environment)
        let multiple_candidates = vec![
            Candidate::Basic("first_item".to_string()),
            Candidate::Basic("second_item".to_string()),
        ];
        let _result = select_item_with_skim(multiple_candidates, None);
        // In a test environment without actual UI, this would return None
        // The important thing is that it doesn't immediately return the first item
        // Since we can't easily test the actual UI behavior in unit tests,
        // we rely on the fact that logic will be tested in integration
    }
}
