use self::cache::CompletionCache;
use crate::completion::display::CompletionConfig;
use crate::completion::ui::{CompletionInteraction, CompletionOutcome, TerminalEventSource};
use crate::dirs::is_executable;
use crate::input::Input;
use crate::lisp::Value;
use crate::repl::Repl;
use anyhow::Result;
use crossterm::{cursor, execute};
use dsh_frecency::ItemStats;
use regex::Regex;
use skim::prelude::*;
use skim::{Skim, SkimItemReceiver, SkimItemSender};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fs::read_dir;
use std::io::stdout;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::Duration;
use std::{process::Command, sync::Arc};
use tracing::debug;
use tracing::warn;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod cache;
mod command;
mod display;
mod dynamic;
mod fuzzy;
mod generator;
mod history;
pub mod integrated;
mod json_loader;
mod parser;
mod ui;

// Re-export from completion module
pub use crate::completion::command::CompletionType;
pub use crate::completion::display::Candidate;
pub use crate::completion::display::CompletionDisplay;
pub use crate::completion::fuzzy::{ScoredCandidate, SmartCompletion};
pub use crate::completion::integrated::IntegratedCompletionEngine;

pub const MAX_RESULT: usize = 500;

const LEGACY_CACHE_TTL_MS: u64 = 3000;

static LEGACY_COMPLETION_CACHE: LazyLock<CompletionCache<Candidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(LEGACY_CACHE_TTL_MS)));

/// Calculate the display width of a Unicode string
/// This accounts for wide characters (like CJK characters and emojis)
#[allow(dead_code)]
fn unicode_display_width(s: &str) -> usize {
    s.width()
}

/// Truncate a Unicode string to fit within the specified display width
#[allow(dead_code)]
fn truncate_to_width(s: &str, max_width: usize) -> String {
    if unicode_display_width(s) <= max_width {
        return s.to_string();
    }

    let mut result = String::new();
    let mut current_width = 0;

    for ch in s.chars() {
        let char_width = ch.width().unwrap_or(0);
        if current_width + char_width > max_width.saturating_sub(1) {
            // Reserve space for ellipsis
            result.push('…');
            break;
        }
        result.push(ch);
        current_width += char_width;
    }

    result
}

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
    // Log the completion candidates before passing them to skim
    debug!(
        "select_item_with_skim: {} candidates provided, query: {:?}",
        items.len(),
        query
    );
    for (i, candidate) in items.iter().enumerate() {
        debug!("Skim candidate {}: {:?}", i, candidate);
    }

    // Configure skim fuzzy finder options
    let options = SkimOptionsBuilder::default()
        .select_1(true) // Only allow selecting one item
        .bind(vec!["Enter:accept".to_string()]) // Use Enter to accept selection
        .query(query.map(|s| s.to_string())) // Optional initial query
        .build()
        .unwrap();

    // Create channels to send items to skim
    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();

    // Send all completion candidates to skim
    for item in items {
        let _ = tx_item.send(Arc::new(item));
    }
    drop(tx_item); // Close the sender to signal no more items

    // Run skim and get selected items
    // When no items are provided, skim will show an empty interface
    // User can still press Enter (with no selection) or ESC to cancel
    let selected = Skim::run_with(&options, Some(rx_item))
        .map(|out| match out.final_key {
            Key::Enter => out.selected_items, // User pressed Enter (may be empty if no items)
            _ => Vec::new(),                  // User cancelled (ESC, Ctrl+C, etc.)
        })
        .unwrap_or_default();

    // Return the selected item if one was chosen
    if !selected.is_empty() {
        let val = selected[0].output().to_string();
        return Some(val);
    }

    // Return None if no items were selected or if no items were provided
    // This is consistent with the "silent failure" behavior when no matches are found
    None
}

// Helper function to get current prompt and input for completion display
fn get_prompt_and_input_for_completion() -> (String, String) {
    // For backward compatibility, return reasonable defaults
    // In practice, the main completion path should use the version with explicit parameters
    ("$ ".to_string(), "".to_string())
}

pub fn select_completion_items(
    items: Vec<Candidate>,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
) -> Option<String> {
    debug!(
        "select_completion_items: items={}, query={:?}, prompt_text='{}', input_text='{}'",
        items.len(),
        query,
        prompt_text,
        input_text
    );
    select_completion_items_with_config(
        items,
        query,
        prompt_text,
        input_text,
        CompletionConfig::default(),
    )
}

fn last_word(s: &str) -> &str {
    s.split_whitespace().last().unwrap_or("")
}

pub fn select_completion_items_with_config(
    items: Vec<Candidate>,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    config: CompletionConfig,
) -> Option<String> {
    debug!(
        "select_completion_items_with_config: items={}, query={:?}, prompt_text='{}', input_text='{}'",
        items.len(),
        query,
        prompt_text,
        input_text
    );

    // Log the actual content of the completion candidates before display
    // for (i, candidate) in items.iter().enumerate() {
    //     debug!("Completion candidate {}: {:?}", i, candidate);
    // }

    // If no completion candidates are available, return None immediately
    // This prevents showing an empty completion UI to the user
    // This is the "silent failure" behavior when no matches are found
    if items.is_empty() {
        debug!("No completion candidates found, returning None");
        return None;
    }

    let mut display =
        CompletionDisplay::new_with_config(items.clone(), prompt_text, input_text, config);

    let mut controller = CompletionInteraction::new(TerminalEventSource);

    // Run the completion interaction loop
    // This displays the completion UI and waits for user selection
    match controller.run(&mut display) {
        // User selected a completion item
        Ok(CompletionOutcome::Submitted(value)) => Some(value),
        Ok(CompletionOutcome::Input(value)) => Some(last_word(input_text).to_owned() + &value),
        // User cancelled (e.g. pressed ESC) or made no selection
        // Both cases return None, maintaining the "silent failure" behavior
        Ok(CompletionOutcome::Cancelled) | Ok(CompletionOutcome::NoSelection) => None,
        // Error occurred during completion interaction
        Err(error) => {
            warn!("Completion interaction failed: {}", error);
            let _ = display.clear_display();
            let _ = execute!(stdout(), cursor::Show);
            None
        }
    }
}

// Backward compatibility function
pub fn select_completion_items_simple(
    items: Vec<Candidate>,
    query: Option<&str>,
) -> Option<String> {
    let (prompt_text, input_text) = get_prompt_and_input_for_completion();
    select_completion_items(items, query, &prompt_text, &input_text)
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

#[allow(dead_code)]
fn completion_from_lisp(input: &Input, repl: &Repl, query: Option<&str>) -> Option<String> {
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
                        return select_completion_items_simple(items, query);
                    }
                    Ok(Value::String(str)) => {
                        return Some(str);
                    }
                    Err(err) => {
                        println!("{err:?}");
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
                return select_completion_items_simple(items, query);
            }
            return None;
        }
    }
    None
}

#[allow(dead_code)]
fn completion_from_current(_input: &Input, repl: &Repl, query: Option<&str>) -> Option<String> {
    debug!("completion_from_current: query={:?}", query);

    let query_str = query.filter(|q| !q.is_empty())?;

    debug!("cache query_str: '{}'", query_str);
    if let Some(hit) = LEGACY_COMPLETION_CACHE.lookup(query_str) {
        debug!(
            "cache hit (simple) for query '{}' (key: '{}', len: {})",
            query_str,
            hit.key,
            hit.candidates.len()
        );
        if hit.exact || !hit.candidates.is_empty() {
            LEGACY_COMPLETION_CACHE.extend_ttl(&hit.key);
            return select_completion_items_simple(hit.candidates, Some(query_str));
        }
    }

    let data = collect_current_context_candidates(repl, query_str)?;

    if data.items.is_empty() {
        return None;
    }

    LEGACY_COMPLETION_CACHE.set(query_str.to_string(), data.items.clone());

    select_completion_items_simple(data.items, data.selection_query.as_deref())
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

/// Enhanced completion with fuzzy matching support
#[allow(dead_code)]
pub fn input_completion_with_fuzzy(
    input: &Input,
    repl: &Repl,
    query: Option<&str>,
    _prompt_text: String,
    _input_text: String,
) -> Option<String> {
    let query_str = query.unwrap_or("");

    // Skip fuzzy completion for very short queries to avoid noise
    if query_str.len() < 2 {
        return None;
    }

    debug!("Starting fuzzy completion for query: '{}'", query_str);

    // Collect all possible completion candidates
    let mut all_candidates = Vec::new();

    // 1. Get command candidates from PATH
    if let Some(word) = get_current_word(input)
        && is_command_position(input)
    {
        let command_candidates = get_command_candidates(&word);
        all_candidates.extend(command_candidates);
    }

    // 2. Get file/directory candidates
    if let Some(word) = get_current_word(input) {
        let file_candidates = get_file_candidates(&word);
        all_candidates.extend(file_candidates);
    }

    // 3. Get history candidates
    if let Some(ref history) = repl.shell.cmd_history
        && let Ok(history) = history.lock()
    {
        let history_candidates: Vec<Candidate> = history
            .sorted(&dsh_frecency::SortMethod::Frecent)
            .iter()
            .take(50) // Limit history candidates for performance
            .map(|item| Candidate::Basic(item.item.clone()))
            .collect();
        all_candidates.extend(history_candidates);
    }

    // 4. Apply fuzzy matching with smart completion
    if !all_candidates.is_empty() {
        let smart_completion = SmartCompletion::new();
        let filtered_candidates = smart_completion.complete(all_candidates, query_str);

        debug!(
            "Found {} fuzzy completion candidates",
            filtered_candidates.len()
        );

        if !filtered_candidates.is_empty() {
            // Use skim to display and select from fuzzy-matched candidates
            return select_item_with_skim(filtered_candidates, Some(query_str));
        }
    }

    None
}

// Backward compatibility function
#[allow(dead_code)]
pub fn input_completion_simple(input: &Input, repl: &Repl, query: Option<&str>) -> Option<String> {
    let (prompt_text, input_text) = get_prompt_and_input_for_completion();
    input_completion(input, repl, query, &prompt_text, &input_text)
}

/// Get the current word being typed for completion
#[allow(dead_code)]
fn get_current_word(input: &Input) -> Option<String> {
    let text = input.as_str();
    let cursor = input.cursor();

    if cursor == 0 || text.is_empty() {
        return None;
    }

    // Find word boundaries
    let mut start = cursor;
    let chars: Vec<char> = text.chars().collect();

    // Move back to find start of current word
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }

    // Extract the current word
    if start < chars.len() {
        let word: String = chars[start..cursor.min(chars.len())].iter().collect();
        if !word.is_empty() {
            return Some(word);
        }
    }

    None
}

/// Check if the cursor is at a command position (beginning of line or after pipe/semicolon)
#[allow(dead_code)]
fn is_command_position(input: &Input) -> bool {
    let text = input.as_str();
    let cursor = input.cursor();

    if cursor == 0 {
        return true;
    }

    // Look for command separators before current position
    let before_cursor = &text[..cursor];

    // Find the last non-whitespace character before cursor
    if let Some(last_char) = before_cursor.chars().rev().find(|c| !c.is_whitespace()) {
        // Command position if after pipe, semicolon, or ampersand
        last_char == '|' || last_char == ';' || last_char == '&'
    } else {
        // If only whitespace before cursor, it's a command position
        true
    }
}

/// Get command candidates from PATH
#[allow(dead_code)]
fn get_command_candidates(_query: &str) -> Vec<Candidate> {
    let mut candidates = Vec::new();

    // Get commands from PATH
    if let Ok(path_var) = std::env::var("PATH") {
        for path_dir in path_var.split(':') {
            if let Ok(entries) = read_dir(path_dir) {
                for entry in entries.flatten() {
                    if let Ok(file_type) = entry.file_type()
                        && file_type.is_file()
                    {
                        let file_name = entry.file_name().to_string_lossy().to_string();
                        if is_executable(&entry) {
                            candidates.push(Candidate::Command {
                                name: file_name,
                                description: "executable".to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

    // Add built-in commands
    let builtins = vec![
        "cd", "pwd", "ls", "echo", "exit", "help", "history", "alias", "export", "unset", "source",
        ".", "exec", "eval", "test", "[",
    ];

    for builtin in builtins {
        candidates.push(Candidate::Command {
            name: builtin.to_string(),
            description: "built-in command".to_string(),
        });
    }

    candidates
}

/// Get file and directory candidates
#[allow(dead_code)]
fn get_file_candidates(query: &str) -> Vec<Candidate> {
    let mut candidates = Vec::new();

    // Determine the directory to search in
    let (search_dir, prefix) = if query.contains('/') {
        let path = Path::new(query);
        if let Some(parent) = path.parent() {
            (
                parent.to_path_buf(),
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            )
        } else {
            (PathBuf::from("."), query.to_string())
        }
    } else {
        (PathBuf::from("."), query.to_string())
    };

    // Read directory entries
    if let Ok(entries) = read_dir(&search_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden files unless query starts with dot
            if file_name.starts_with('.') && !prefix.starts_with('.') {
                continue;
            }

            if let Ok(file_type) = entry.file_type() {
                let full_path = if search_dir == Path::new(".") {
                    file_name.clone()
                } else {
                    search_dir.join(&file_name).to_string_lossy().to_string()
                };

                if file_type.is_dir() {
                    candidates.push(Candidate::File {
                        path: full_path,
                        is_dir: true,
                    });
                } else {
                    candidates.push(Candidate::File {
                        path: full_path,
                        is_dir: false,
                    });
                }
            }
        }
    }

    candidates
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
                        println!("{err:?}");
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
    if cmd.starts_with('/') {
        let cmd_path = std::path::Path::new(cmd);
        if cmd_path.exists() && cmd_path.is_file() {
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
        let mut cmds = get_executables(path, cmd);
        list.append(&mut cmds);
    }

    // Deduplicate commands from multiple PATH directories
    deduplicate_candidates(list)
}

fn get_executables(dir: &str, name: &str) -> Vec<Candidate> {
    let mut list = Vec::new();
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

                // Apply prefix filtering for command names
                if file_name.starts_with(name) && is_file && is_executable(&entry) {
                    list.push(Candidate::Item(
                        file_name.to_string(),
                        "(command)".to_string(),
                    ));
                }
            }
        }
        Err(_err) => {}
    }
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
// Pre-compiled regex for whitespace splitting - more efficient than split_whitespace for complex patterns
#[allow(dead_code)]
static WHITESPACE_SPLIT_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\s+").unwrap());

fn replace_space(s: &str) -> String {
    WHITESPACE_REGEX.replace_all(s, "_").to_string()
}

/// Advanced completion engine that combines multiple completion strategies
///
/// This is the new integrated completion system that replaces the old AdvancedCompletion.
/// It provides enhanced functionality with JSON-based command completion, improved UI,
/// and better integration between different completion sources.
///
/// Legacy compatibility functions for AdvancedCompletion
impl IntegratedCompletionEngine {
    /// Get completion for a specific command (legacy compatibility)
    #[allow(dead_code)]
    pub async fn complete_command(
        &self,
        command: &str,
        args: &[String],
        current_dir: &std::path::Path,
    ) -> Vec<Candidate> {
        // Reconstruct input from command and args
        let input = if args.is_empty() {
            command.to_string()
        } else {
            format!("{} {}", command, args.join(" "))
        };

        let enhanced_candidates = self
            .complete(&input, input.len(), current_dir, MAX_RESULT)
            .await;

        // Convert enhanced candidates back to legacy format
        enhanced_candidates
            .into_iter()
            .map(|ec| match ec.candidate_type {
                crate::completion::integrated::CandidateType::File => Candidate::File {
                    path: ec.text,
                    is_dir: false,
                },
                crate::completion::integrated::CandidateType::Directory => Candidate::File {
                    path: ec.text,
                    is_dir: true,
                },
                crate::completion::integrated::CandidateType::SubCommand => Candidate::Command {
                    name: ec.text,
                    description: ec.description.unwrap_or_default(),
                },
                crate::completion::integrated::CandidateType::LongOption => Candidate::Option {
                    name: ec.text,
                    description: ec.description.unwrap_or_default(),
                },
                _ => Candidate::Item(ec.text, ec.description.unwrap_or_default()),
            })
            .collect()
    }

    /// Update history with executed command (legacy compatibility)
    #[allow(dead_code)]
    pub fn update_history(
        &mut self,
        _command: &str,
        _current_dir: &std::path::Path,
    ) -> anyhow::Result<()> {
        // This is a simplified implementation for compatibility
        Ok(())
    }

    /// Get fuzzy matches for a query (legacy compatibility)
    #[allow(dead_code)]
    pub fn fuzzy_search(&self, candidates: Vec<Candidate>, query: &str) -> Vec<ScoredCandidate> {
        use crate::completion::integrated::{CandidateSource, CandidateType, EnhancedCandidate};

        // Convert to enhanced candidates and back for fuzzy matching
        let enhanced: Vec<EnhancedCandidate> = candidates
            .into_iter()
            .map(|c| match c {
                Candidate::Item(text, desc) => EnhancedCandidate {
                    text,
                    description: Some(desc),
                    candidate_type: CandidateType::Generic,
                    priority: CandidateSource::Context.base_priority().saturating_sub(30),
                    source: CandidateSource::Context,
                },
                Candidate::File { path, is_dir } => EnhancedCandidate {
                    text: path,
                    description: None,
                    candidate_type: if is_dir {
                        CandidateType::Directory
                    } else {
                        CandidateType::File
                    },
                    priority: if is_dir {
                        CandidateSource::Context.base_priority().saturating_sub(30)
                    } else {
                        CandidateSource::Context.base_priority().saturating_sub(40)
                    },
                    source: CandidateSource::Context,
                },
                Candidate::Path(path) => EnhancedCandidate {
                    text: path,
                    description: None,
                    candidate_type: CandidateType::File,
                    priority: CandidateSource::Context.base_priority().saturating_sub(40),
                    source: CandidateSource::Context,
                },
                Candidate::Basic(text) => EnhancedCandidate {
                    text,
                    description: None,
                    candidate_type: CandidateType::Generic,
                    priority: CandidateSource::Context.base_priority().saturating_sub(50),
                    source: CandidateSource::Context,
                },
                Candidate::Command { name, description } => EnhancedCandidate {
                    text: name,
                    description: Some(description),
                    candidate_type: CandidateType::SubCommand,
                    priority: CandidateSource::Context.base_priority(),
                    source: CandidateSource::Context,
                },
                Candidate::Option { name, description } => EnhancedCandidate {
                    text: name,
                    description: Some(description),
                    candidate_type: CandidateType::LongOption,
                    priority: CandidateSource::Context.base_priority().saturating_sub(10),
                    source: CandidateSource::Context,
                },
                Candidate::GitBranch { name, .. } => EnhancedCandidate {
                    text: name,
                    description: None,
                    candidate_type: CandidateType::Generic,
                    priority: CandidateSource::Context.base_priority().saturating_sub(20),
                    source: CandidateSource::Context,
                },
                Candidate::History { command, .. } => EnhancedCandidate {
                    text: command,
                    description: None,
                    candidate_type: CandidateType::Generic,
                    priority: CandidateSource::History.base_priority().saturating_sub(20),
                    source: CandidateSource::History,
                },
            })
            .collect();

        // Simple fuzzy matching implementation for compatibility
        enhanced
            .into_iter()
            .filter(|c| c.text.to_lowercase().contains(&query.to_lowercase()))
            .map(|c| ScoredCandidate {
                candidate: match c.candidate_type {
                    CandidateType::File => Candidate::File {
                        path: c.text,
                        is_dir: false,
                    },
                    CandidateType::Directory => Candidate::File {
                        path: c.text,
                        is_dir: true,
                    },
                    CandidateType::SubCommand => Candidate::Command {
                        name: c.text,
                        description: c.description.unwrap_or_default(),
                    },
                    CandidateType::LongOption => Candidate::Option {
                        name: c.text,
                        description: c.description.unwrap_or_default(),
                    },
                    _ => Candidate::Item(c.text, c.description.unwrap_or_default()),
                },
                score: c.priority as i64,
                matched_indices: vec![],
            })
            .collect()
    }
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

    //     #[test]
    //     fn test_completion() -> Result<()> {
    //         init();
    //         let p = path_completion_prefix(".")?;
    //         assert_eq!(None, p);
    //
    //         let p = path_completion_prefix("./")?;
    //         assert_eq!(None, p);
    //
    //         let p = path_completion_prefix("./sr")?;
    //         assert_eq!(Some("./src/".to_string()), p);
    //
    //         let p = path_completion_prefix("sr")?;
    //         assert_eq!(Some("src/".to_string()), p);
    //
    //         // let p = path_completion_first("src/b")?;
    //         // assert_eq!(Some("src/builtin/".to_string()), p);
    //
    //         let p = path_completion_prefix("/")?;
    //         assert_eq!(None, p);
    //
    //         let p = path_completion_prefix("/s")?;
    //         assert_eq!(Some("/sbin/".to_string()), p);
    //
    //         let p = path_completion_prefix("/usr/b")?;
    //         assert_eq!(Some("/usr/bin/".to_string()), p);
    //
    //         let p = path_completion_prefix("~/.lo")?;
    //         assert_eq!(Some("~/.local/".to_string()), p);
    //
    //         let p = path_completion_prefix("~/.config/gi")?;
    //         // Environment-dependent, so verify git-related directory exists
    //         assert!(p.is_some());
    //         assert!(p.unwrap().starts_with("~/.config/git"));
    //
    //         Ok(())
    //     }

    //     #[test]
    //     fn test_file_completion_with_prefix_filter() {
    //         init();
    //
    //         // Create test directory structure in memory for testing
    //         // This test verifies the prefix filtering logic
    //         let test_files = [
    //             ("test_file.txt", true),
    //             ("test_script.sh", true),
    //             ("another_file.txt", true),
    //             ("test_dir", false),
    //             ("temp_dir", false),
    //         ];
    //
    //         // Test prefix filtering logic
    //         let filtered_files: Vec<_> = test_files
    //             .iter()
    //             .filter(|(name, _)| name.starts_with("test"))
    //             .collect();
    //
    //         assert_eq!(filtered_files.len(), 3);
    //         assert!(
    //             filtered_files
    //                 .iter()
    //                 .any(|(name, _)| *name == "test_file.txt")
    //         );
    //         assert!(
    //             filtered_files
    //                 .iter()
    //                 .any(|(name, _)| *name == "test_script.sh")
    //         );
    //         assert!(filtered_files.iter().any(|(name, _)| *name == "test_dir"));
    //         assert!(
    //             !filtered_files
    //                 .iter()
    //                 .any(|(name, _)| *name == "another_file.txt")
    //         );
    //         assert!(!filtered_files.iter().any(|(name, _)| *name == "temp_dir"));
    //     }

    //     #[test]
    //     fn test_command_completion_with_prefix_filter() {
    //         init();
    //
    //         // Test command prefix filtering logic
    //         let test_commands = ["git", "grep", "gcc", "ls", "cat", "grep-test"];
    //
    //         let prefix = "g";
    //         let filtered_commands: Vec<_> = test_commands
    //             .iter()
    //             .filter(|cmd| cmd.starts_with(prefix))
    //             .collect();
    //
    //         assert_eq!(filtered_commands.len(), 4);
    //         assert!(filtered_commands.contains(&&"git"));
    //         assert!(filtered_commands.contains(&&"grep"));
    //         assert!(filtered_commands.contains(&&"gcc"));
    //         assert!(filtered_commands.contains(&&"grep-test"));
    //         assert!(!filtered_commands.contains(&&"ls"));
    //         assert!(!filtered_commands.contains(&&"cat"));
    //
    //         // Test with more specific prefix
    //         let prefix = "gr";
    //         let filtered_commands: Vec<_> = test_commands
    //             .iter()
    //             .filter(|cmd| cmd.starts_with(prefix))
    //             .collect();
    //
    //         assert_eq!(filtered_commands.len(), 2);
    //         assert!(filtered_commands.contains(&&"grep"));
    //         assert!(filtered_commands.contains(&&"grep-test"));
    //         assert!(!filtered_commands.contains(&&"git"));
    //         assert!(!filtered_commands.contains(&&"gcc"));
    //     }

    //     #[test]
    //     #[ignore]
    //     fn test_select_item() {
    //         init();
    //         let items: Vec<Candidate> = vec![
    //             Candidate::Basic("test1".to_string()),
    //             Candidate::Basic("test2".to_string()),
    //         ];
    //
    //         let a = select_completion_items_simple(items, Some("test"));
    //         assert_eq!("test1", a.unwrap());
    //     }

    //     #[test]
    //     fn test_completion_config() {
    //         init();
    //
    //         // Test default config
    //         let config = CompletionConfig::default();
    //         assert_eq!(config.max_items, 30);
    //         assert_eq!(
    //             config.more_items_message_template,
    //             "...and {} more items available"
    //         );
    //         assert!(config.show_item_count);
    //
    //         // Test custom config
    //         let config = CompletionConfig::new()
    //             .with_max_items(10)
    //             .with_message_template("There are {} more items")
    //             .with_item_count_display(false);
    //
    //         assert_eq!(config.max_items, 10);
    //         assert_eq!(
    //             config.more_items_message_template,
    //             "There are {} more items"
    //         );
    //         assert!(!config.show_item_count);
    //
    //         // Test message formatting
    //         let message = config.format_more_items_message(25);
    //         assert_eq!(message, "There are 25 more items");
    //     }

    //     #[test]
    //     fn test_completion_display_with_limit() {
    //         init();
    //
    //         // Create more than 30 candidates
    //         let mut candidates = Vec::new();
    //         for i in 0..50 {
    //             candidates.push(Candidate::Basic(format!("item_{:02}", i)));
    //         }
    //
    //         let config = CompletionConfig::default();
    //         let comp_display = CompletionDisplay::with_default_config();
    //
    //         // Should have 30 items + 1 message item
    //         assert_eq!(comp_display.candidates.len(), 31);
    //         assert!(comp_display.has_more_items);
    //         assert_eq!(comp_display.total_items_count, 50);
    //
    //         // Last item should be the message
    //         let last_item = &comp_display.candidates[30];
    //         assert!(last_item.get_display_name().starts_with("📋"));
    //         assert!(last_item.get_display_name().contains("20 more items"));
    //     }

    //     #[test]
    //     fn test_completion_display_no_limit() {
    //         init();
    //
    //         // Create less than 30 candidates
    //         let mut candidates = Vec::new();
    //         for i in 0..10 {
    //             candidates.push(Candidate::Basic(format!("item_{:02}", i)));
    //         }
    //
    //         let config = CompletionConfig::default();
    //         let comp_display = CompletionDisplay::with_default_config();
    //
    //         // Should have exactly 10 items, no message
    //         assert_eq!(comp_display.candidates.len(), 10);
    //         assert!(!comp_display.has_more_items);
    //         assert_eq!(comp_display.total_items_count, 10);
    //     }

    //     #[test]
    //     fn test_completion_display_space_calculation() {
    //         init();
    //
    //         // Create many candidates to test space requirements
    //         let mut candidates = Vec::new();
    //         for i in 0..100 {
    //             candidates.push(Candidate::Basic(format!("item_{:03}", i)));
    //         }
    //
    //         let config = CompletionConfig::default().with_max_items(50);
    //         let comp_display =
    //             CompletionDisplay::with_default_config();
    //
    //         // Should limit to 50 items + 1 message
    //         assert_eq!(comp_display.candidates.len(), 51);
    //         assert!(comp_display.has_more_items);
    //         assert_eq!(comp_display.total_items_count, 100);
    //
    //         // Check that total_rows is calculated correctly
    //         let expected_rows = comp_display
    //             .candidates
    //             .len()
    //             .div_ceil(comp_display.items_per_row);
    //         assert_eq!(comp_display.total_rows, expected_rows);
    //
    //         debug!(
    //             "Display has {} rows with {} items per row",
    //             comp_display.total_rows, comp_display.items_per_row
    //         );
    //     }

    //     #[test]
    //     fn test_completion_display_small_terminal() {
    //         init();
    //
    //         // Test with a scenario that would require space creation
    //         let mut candidates = Vec::new();
    //         for i in 0..20 {
    //             candidates.push(Candidate::Basic(format!("command_{}", i)));
    //         }
    //
    //         let config = CompletionConfig::default();
    //         let comp_display =
    //             CompletionDisplay::with_default_config();
    //
    //         // Verify the display is properly configured
    //         assert_eq!(comp_display.candidates.len(), 20);
    //         assert!(!comp_display.has_more_items);
    //         assert!(comp_display.total_rows > 0);
    //
    //         // The ensure_display_space method should handle space creation
    //         // This is mainly tested through integration testing
    //     }
}
#[cfg(test)]
mod fuzzy_integration_tests {
    #[allow(dead_code)]
    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    //     #[test]
    //     fn test_get_current_word() {
    //         // Test basic word extraction
    //         let config = InputConfig::default();
    //         let mut input = Input::new(config);
    //         input.reset("git commit".to_string());
    //         input.move_to_end();
    //
    //         let word = get_current_word(&input);
    //         assert_eq!(word, Some("commit".to_string()));
    //     }

    //     #[test]
    //     fn test_get_current_word_partial() {
    //         let config = InputConfig::default();
    //         let mut input = Input::new(config);
    //         input.reset("git com".to_string());
    //         input.move_to_end();
    //
    //         let word = get_current_word(&input);
    //         assert_eq!(word, Some("com".to_string()));
    //     }

    //     #[test]
    //     fn test_is_command_position() {
    //         let config = InputConfig::default();
    //         let mut input = Input::new(config);
    //
    //         // Test beginning of line
    //         input.reset("git".to_string());
    //         input.move_to_begin();
    //         assert!(is_command_position(&input));
    //
    //         // Test in middle of command (not command position)
    //         input.move_to_end();
    //         assert!(!is_command_position(&input)); // "git" is not a command position at the end
    //
    //         // Test after pipe
    //         input.reset("ls | grep".to_string());
    //         input.move_to_end();
    //         assert!(!is_command_position(&input)); // At end of "grep", not command position
    //
    //         // Test right after pipe character
    //         input.reset("ls | ".to_string());
    //         input.move_to_end();
    //         assert!(is_command_position(&input)); // After pipe and space, is command position
    //     }

    //     #[test]
    //     fn test_get_command_candidates() {
    //         let candidates = get_command_candidates("g");
    //
    //         // Should contain built-in commands
    //         let names: Vec<String> = candidates
    //             .iter()
    //             .map(|c| c.get_display_name().to_string())
    //             .collect();
    //
    //         assert!(names.contains(&"cd".to_string()));
    //         assert!(names.contains(&"echo".to_string()));
    //     }

    //     #[test]
    //     fn test_get_file_candidates() {
    //         let candidates = get_file_candidates(".");
    //
    //         // Should find some files/directories in current directory
    //         assert!(!candidates.is_empty());
    //
    //         // Check that we have both files and directories
    //         let has_files = candidates
    //             .iter()
    //             .any(|c| matches!(c, Candidate::File { is_dir: false, .. }));
    //         let has_dirs = candidates
    //             .iter()
    //             .any(|c| matches!(c, Candidate::File { is_dir: true, .. }));
    //
    //         // At least one should be true (current directory should have some content)
    //         assert!(has_files || has_dirs);
    //     }

    //     #[test]
    //     fn test_unicode_display_width() {
    //         init();
    //
    //         // ASCII character test
    //         assert_eq!(unicode_display_width("hello"), 5);
    //
    //         // Japanese character test (full-width characters are 2 character widths)
    //         assert_eq!(unicode_display_width("こんにちは"), 10);
    //
    //         // Emoji test
    //         assert_eq!(unicode_display_width("🐕"), 2);
    //         assert_eq!(unicode_display_width("⚡"), 2);
    //         assert_eq!(unicode_display_width("📁"), 2);
    //
    //         // Mixed test
    //         assert_eq!(unicode_display_width("hello世界🐕"), 5 + 4 + 2); // 11
    //     }

    //     #[test]
    //     fn test_truncate_to_width() {
    //         init();
    //
    //         // ASCII character truncation test
    //         assert_eq!(truncate_to_width("hello_world", 5), "hell…");
    //         assert_eq!(truncate_to_width("hello", 10), "hello");
    //
    //         // Japanese character truncation test
    //         assert_eq!(truncate_to_width("こんにちは", 6), "こん…"); // 4 char width + 1 char width (…) = 5 char width
    //
    //         // Emoji truncation test
    //         assert_eq!(truncate_to_width("🐕🚀⚡", 4), "🐕…"); // 2 char width + 1 char width (…) = 3 char width
    //
    //         // Mixed truncation test
    //         assert_eq!(truncate_to_width("hello世界", 8), "hello世…"); // 5 char width + 2 char width + 1 char width (…) = 8 char width
    //     }

    //     #[test]
    //     fn test_candidate_formatted_display_unicode() {
    //         init();
    //
    //         // Test Japanese filename
    //         let candidate = Candidate::File {
    //             path: "japanese_file.txt".to_string(),
    //             is_dir: false,
    //         };
    //
    //         let formatted = candidate.get_formatted_display(20);
    //         let display_width = unicode_display_width(&formatted);
    //
    //         // Verify display width is within specified width
    //         assert!(display_width <= 20);
    //
    //         // Verify emoji is included
    //         assert!(formatted.contains("📄"));
    //     }
}
