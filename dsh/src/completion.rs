use crate::dirs::is_executable;
use crate::environment::get_data_file;
use crate::input::Input;
use crate::lisp::Value;
use crate::repl::Repl;
use anyhow::Result;

// Advanced completion modules
pub mod command;
pub mod context;
pub mod display;
pub mod fuzzy;
pub mod generator;
pub mod history;
pub mod integrated;
pub mod json_loader;
pub mod parser;

pub use self::context::CommandCompleter;
pub use self::display::CompletionDisplay;
pub use self::fuzzy::{ScoredCandidate, SmartCompletion};
pub use self::integrated::IntegratedCompletionEngine;
use dsh_frecency::ItemStats;
use regex::Regex;
use serde::{Deserialize, Serialize};
use skim::prelude::*;
use skim::{Skim, SkimItemReceiver, SkimItemSender};
use std::borrow::Cow;
use std::fs::{File, create_dir_all, read_dir, remove_file};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::{process::Command, sync::Arc};
use tracing::debug;
use tracing::warn;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// Completion display configuration
const MAX_COMPLETION_ITEMS: usize = 30;

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
pub struct CompletionConfig {
    pub max_items: usize,
    pub more_items_message_template: String,
    pub show_item_count: bool,
}

impl Default for CompletionConfig {
    fn default() -> Self {
        Self {
            max_items: MAX_COMPLETION_ITEMS,
            more_items_message_template: "...and {} more items available".to_string(),
            show_item_count: true,
        }
    }
}

impl CompletionConfig {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(dead_code)]
    pub fn with_max_items(mut self, max_items: usize) -> Self {
        self.max_items = max_items;
        self
    }

    #[allow(dead_code)]
    pub fn with_message_template<S: Into<String>>(mut self, template: S) -> Self {
        self.more_items_message_template = template.into();
        self
    }

    #[allow(dead_code)]
    pub fn with_item_count_display(mut self, show: bool) -> Self {
        self.show_item_count = show;
        self
    }

    #[allow(dead_code)]
    pub fn format_more_items_message(&self, remaining_count: usize) -> String {
        if self.more_items_message_template.contains("{}") {
            self.more_items_message_template
                .replace("{}", &remaining_count.to_string())
        } else {
            format!("{} ({})", self.more_items_message_template, remaining_count)
        }
    }
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, PartialOrd, Eq, Ord)]
pub enum Candidate {
    Item(String, String), // output, description
    Path(String),
    Basic(String),
    // Context-aware completion types
    Command {
        name: String,
        description: String,
    },
    Option {
        name: String,
        description: String,
    },
    File {
        path: String,
        is_dir: bool,
    },
    GitBranch {
        name: String,
        is_current: bool,
    },
    NpmScript {
        name: String,
    },
    History {
        command: String,
        frequency: u32,
        last_used: i64,
    },
}

impl Candidate {
    /// Get the type character for display
    #[allow(dead_code)]
    pub fn get_type_char(&self) -> char {
        match self {
            Candidate::Item(_, desc) => {
                if desc.contains("command") {
                    '⚡' // Command - lightning bolt
                } else if desc.contains("file") {
                    '📄' // File - document
                } else if desc.contains("directory") {
                    '📁' // Directory - folder
                } else {
                    '⚙' // Option or other - gear
                }
            }
            Candidate::Path(path) => {
                if path.ends_with('/') {
                    '📁' // Directory - folder
                } else {
                    '📄' // File - document
                }
            }
            Candidate::Basic(_) => '🔹', // Basic - small blue diamond
            Candidate::Command { .. } => '⚡', // Command - lightning bolt
            Candidate::Option { .. } => '⚙', // Option - gear
            Candidate::File { is_dir, .. } => {
                if *is_dir {
                    '📁' // Directory - folder
                } else {
                    '📄' // File - document
                }
            }
            Candidate::GitBranch { .. } => '🌿', // Git branch - herb/branch
            Candidate::NpmScript { .. } => '📜', // Script - scroll
            Candidate::History { .. } => '🕒',   // History - clock
        }
    }

    /// Get the display name (without description)
    pub fn get_display_name(&self) -> &str {
        match self {
            Candidate::Item(name, _) => name,
            Candidate::Path(path) => path,
            Candidate::Basic(basic) => basic,
            Candidate::Command { name, .. } => name,
            Candidate::Option { name, .. } => name,
            Candidate::File { path, .. } => path,
            Candidate::GitBranch { name, .. } => name,
            Candidate::NpmScript { name } => name,
            Candidate::History { command, .. } => command,
        }
    }

    /// Get formatted display string with type character
    #[allow(dead_code)]
    pub fn get_formatted_display(&self, width: usize) -> String {
        let type_char = self.get_type_char();
        let name = self.get_display_name();

        // Calculate the width needed for the type character (emoji)
        let type_char_width = type_char.width().unwrap_or(2);

        // Calculate maximum width available for the name
        // Format: "emoji name" with proper spacing
        let max_name_width = width.saturating_sub(type_char_width + 1); // type_char + " "

        // Truncate name if it's too long for the available width
        let display_name = if unicode_display_width(name) > max_name_width {
            truncate_to_width(name, max_name_width)
        } else {
            name.to_string()
        };

        // Calculate padding needed to align columns properly
        let name_display_width = unicode_display_width(&display_name);
        let total_content_width = type_char_width + 1 + name_display_width; // type_char + " " + name
        let padding_needed = width.saturating_sub(total_content_width);

        format!(
            "{} {}{}",
            type_char,
            display_name,
            " ".repeat(padding_needed)
        )
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
        .unwrap();
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
    fn output(&self) -> Cow<str> {
        match self {
            Candidate::Item(x, _) => Cow::Borrowed(x),
            Candidate::Path(p) => Cow::Borrowed(p),
            Candidate::Basic(x) => Cow::Borrowed(x),
            Candidate::Command { name, .. } => Cow::Borrowed(name),
            Candidate::Option { name, .. } => Cow::Borrowed(name),
            Candidate::File { path, .. } => Cow::Borrowed(path),
            Candidate::GitBranch { name, .. } => Cow::Borrowed(name),
            Candidate::NpmScript { name } => Cow::Borrowed(name),
            Candidate::History { command, .. } => Cow::Borrowed(command),
        }
    }

    fn text(&self) -> Cow<str> {
        match self {
            Candidate::Item(x, y) => {
                let desc = format!("{0:<30} {1}", x, y);
                Cow::Owned(desc)
            }
            Candidate::Path(p) => Cow::Borrowed(p),
            Candidate::Basic(x) => Cow::Borrowed(x),
            Candidate::Command { name, description } => {
                let desc = format!("{0:<30} {1}", name, description);
                Cow::Owned(desc)
            }
            Candidate::Option { name, description } => {
                let desc = format!("{0:<30} {1}", name, description);
                Cow::Owned(desc)
            }
            Candidate::File { path, is_dir } => {
                let type_indicator = if *is_dir { "/" } else { "" };
                Cow::Owned(format!("{}{}", path, type_indicator))
            }
            Candidate::GitBranch { name, is_current } => {
                let indicator = if *is_current { " (current)" } else { "" };
                Cow::Owned(format!("{}{}", name, indicator))
            }
            Candidate::NpmScript { name } => Cow::Owned(format!("{0:<30} npm script", name)),
            Candidate::History {
                command, frequency, ..
            } => {
                let desc = format!("{0:<30} used {1} times", command, frequency);
                Cow::Owned(desc)
            }
        }
    }
}

pub fn select_item_with_skim(items: Vec<Candidate>, query: Option<&str>) -> Option<String> {
    let options = SkimOptionsBuilder::default()
        .select_1(true)
        .bind(vec!["Enter:accept".to_string()])
        .query(query.map(|s| s.to_string()))
        .build()
        .unwrap();

    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
    for item in items {
        let _ = tx_item.send(Arc::new(item));
    }
    drop(tx_item);

    let selected = Skim::run_with(&options, Some(rx_item))
        .map(|out| match out.final_key {
            Key::Enter => out.selected_items,
            _ => Vec::new(),
        })
        .unwrap_or_default();

    if !selected.is_empty() {
        let val = selected[0].output().to_string();
        return Some(val);
    }

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
    _query: Option<&str>,
    _prompt_text: &str,
    _input_text: &str,
    _config: CompletionConfig,
) -> Option<String> {
    if items.is_empty() {
        return None;
    }

    let _display = CompletionDisplay::with_default_config();

    // TODO: Implement interactive display with new CompletionDisplay API
    // For now, return None to disable interactive completion
    None

    // TODO: Implement interactive completion loop with new API
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
                        println!("{:?}", err);
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
    let lisp_engine = Rc::clone(&repl.shell.lisp_engine);
    let environment = Arc::clone(&lisp_engine.borrow().shell_env);

    // 2 . try completion
    if let Some(query_str) = query {
        // check path
        let current = std::env::current_dir().unwrap_or_else(|e| {
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

        let expand_path = shellexpand::tilde(&query_str);
        let expand = expand_path.as_ref();
        let path = Path::new(expand);

        let (path, path_query, only_path) = if path.is_dir() {
            (path, "", true)
        } else if let Some(parent) = path.parent() {
            let parent = Path::new(parent);
            let has_parent = !parent.as_os_str().is_empty();
            if let Some(file_name) = &path.file_name() {
                (parent, file_name.to_str().unwrap(), has_parent)
            } else {
                (path, "", has_parent)
            }
        } else {
            (current.as_path(), query_str, false)
        };

        let canonical_path = if let Ok(path) = path.canonicalize() {
            path
        } else {
            std::env::current_dir().unwrap_or_else(|e| {
                warn!("Failed to get current directory for canonicalization: {}, using home directory", e);
                std::env::var("HOME").map(std::path::PathBuf::from).ok().unwrap_or_else(|| {
                    warn!("Failed to get home directory, using root");
                    std::path::PathBuf::from("/")
                })
            })
        };
        let path_str = canonical_path.display().to_string();

        // path - Apply prefix filtering for file completions
        let mut items = if path_query.is_empty() {
            get_file_completions(path_str.as_str(), path.to_str().unwrap())
        } else {
            get_file_completions_with_filter(
                path_str.as_str(),
                path.to_str().unwrap(),
                Some(path_query),
            )
        };

        if !only_path {
            let mut cmds_items = get_commands(&environment.read().paths, query_str);
            items.append(&mut cmds_items);
        }
        select_completion_items_simple(items, Some(path_query))
    } else {
        None
    }
}

fn completion_from_chatgpt(input: &Input, repl: &Repl, _query: Option<&str>) -> Option<String> {
    let lisp_engine = Rc::clone(&repl.shell.lisp_engine);
    let environment = Arc::clone(&lisp_engine.borrow().shell_env);

    // ChatGPT Completion
    if let Some(api_key) = environment
        .read()
        .variables
        .get("OPEN_AI_API_KEY")
        .map(|val| val.to_string())
    {
        debug!("ChatGPT completion input:{:?}", input);
        // TODO displaying the inquiring mark

        match ChatGPTCompletion::new(api_key) {
            Ok(mut processor) => match processor.completion(input.as_str()) {
                Ok(res) => {
                    return res;
                }
                Err(err) => {
                    eprintln!("{:?}", err);
                }
            },
            Err(err) => {
                eprintln!("{:?}", err);
            }
        }
    }

    None
}

pub fn input_completion(
    input: &Input,
    repl: &Repl,
    query: Option<&str>,
    prompt_text: &str,
    input_text: &str,
) -> Option<String> {
    // Use original completion logic only (fuzzy completion removed)
    let res = completion_from_lisp_with_prompt(input, repl, query, prompt_text, input_text);
    if res.is_some() {
        return res;
    }
    let res = completion_from_current_with_prompt(input, repl, query, prompt_text, input_text);
    if res.is_some() {
        return res;
    }
    if input.can_execute {
        let res = completion_from_chatgpt(input, repl, query);
        if res.is_some() {
            return res;
        }
    }
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
    if let Some(word) = get_current_word(input) {
        if is_command_position(input) {
            let command_candidates = get_command_candidates(&word);
            all_candidates.extend(command_candidates);
        }
    }

    // 2. Get file/directory candidates
    if let Some(word) = get_current_word(input) {
        let file_candidates = get_file_candidates(&word);
        all_candidates.extend(file_candidates);
    }

    // 3. Get history candidates
    if let Some(ref history) = repl.shell.cmd_history {
        if let Ok(history) = history.lock() {
            let history_candidates: Vec<Candidate> = history
                .sorted(&dsh_frecency::SortMethod::Frecent)
                .iter()
                .take(50) // Limit history candidates for performance
                .map(|item| Candidate::Basic(item.item.clone()))
                .collect();
            all_candidates.extend(history_candidates);
        }
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
                    if let Ok(file_type) = entry.file_type() {
                        if file_type.is_file() {
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
                        println!("{:?}", err);
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
    let lisp_engine = Rc::clone(&repl.shell.lisp_engine);
    let environment = Arc::clone(&lisp_engine.borrow().shell_env);

    // 2 . try completion
    if let Some(query_str) = query {
        // check path
        let current = std::env::current_dir().unwrap_or_else(|e| {
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

        let expand_path = shellexpand::tilde(&query_str);
        let expand = expand_path.as_ref();
        let path = Path::new(expand);

        let (path, path_query, only_path) = if path.is_dir() {
            (path, "", true)
        } else if let Some(parent) = path.parent() {
            let parent = Path::new(parent);
            let has_parent = !parent.as_os_str().is_empty();
            if let Some(file_name) = &path.file_name() {
                (parent, file_name.to_str().unwrap(), has_parent)
            } else {
                (path, "", has_parent)
            }
        } else {
            (current.as_path(), query_str, false)
        };

        let canonical_path = if let Ok(path) = path.canonicalize() {
            path
        } else {
            match std::env::current_dir() {
                Ok(dir) => dir,
                Err(_) => return None, // Return None if we can't get current directory
            }
        };
        let path_str = canonical_path.display().to_string();

        // path - Apply prefix filtering for file completions
        let mut items = if path_query.is_empty() {
            get_file_completions(path_str.as_str(), path.to_str().unwrap())
        } else {
            get_file_completions_with_filter(
                path_str.as_str(),
                path.to_str().unwrap(),
                Some(path_query),
            )
        };

        if !only_path {
            let mut cmds_items = get_commands(&environment.read().paths, query_str);
            items.append(&mut cmds_items);
        }
        select_completion_items(items, Some(path_query), prompt_text, input_text)
    } else {
        None
    }
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
    list
}

fn get_executables(dir: &str, name: &str) -> Vec<Candidate> {
    let mut list = Vec::new();
    match read_dir(dir) {
        Ok(entries) => {
            let mut entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
            entries.sort_by_key(|x| x.file_name());

            for entry in entries {
                let buf = entry.file_name();
                let file_name = buf.to_str().unwrap();
                let is_file = entry.file_type().unwrap().is_file();
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

fn get_file_completions(dir: &str, prefix: &str) -> Vec<Candidate> {
    get_file_completions_with_filter(dir, prefix, None)
}

fn get_file_completions_with_filter(
    dir: &str,
    prefix: &str,
    filter_prefix: Option<&str>,
) -> Vec<Candidate> {
    let mut list = Vec::new();
    let prefix = if !prefix.is_empty() && !prefix.ends_with('/') {
        format!("{}/", prefix)
    } else {
        prefix.to_string()
    };

    match read_dir(dir) {
        Ok(entries) => {
            let mut entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
            entries.sort_by_key(|x| x.file_name());

            for entry in entries {
                let buf = entry.file_name();
                let file_name = buf.to_str().unwrap();
                let is_file = entry.file_type().unwrap().is_file();

                // Apply prefix filter if provided
                if let Some(filter) = filter_prefix {
                    if !file_name.starts_with(filter) {
                        continue;
                    }
                }

                if is_file {
                    list.push(Candidate::Item(
                        format!("{}{}", prefix, file_name),
                        "(file)".to_string(),
                    ));
                } else {
                    list.push(Candidate::Item(
                        format!("{}{}", prefix, file_name),
                        "(directory)".to_string(),
                    ));
                }
            }
        }
        Err(_err) => {}
    }
    list
}

/// ChatGPT completion structure
#[derive(Debug)]
pub struct ChatGPTCompletion {
    api_key: String,
    store_path: PathBuf,
}

fn replace_space(s: &str) -> String {
    let re = Regex::new(r"\s+").unwrap();
    re.replace_all(s, "_").to_string()
}

impl ChatGPTCompletion {
    pub fn new(api_key: String) -> Result<Self> {
        let store_path = get_data_file("completions")?;
        create_dir_all(&store_path)?;
        Ok(ChatGPTCompletion {
            api_key,
            store_path,
        })
    }

    pub fn completion(&mut self, cmd: &str) -> Result<Option<String>> {
        let file_name = replace_space(cmd.trim());
        debug!("completion file name : {}", file_name);
        let completion_file_path = self.store_path.join(file_name + ".json");

        let items = if completion_file_path.exists() {
            let open_file = File::open(&completion_file_path)?;
            let reader = BufReader::new(open_file);
            let items: Vec<Candidate> = serde_json::from_reader(reader)?;
            items
        } else {
            let client = dsh_chatgpt::ChatGptClient::new(self.api_key.to_string())?;
            let content = format!(
                r#"
You are a talented software engineer.
You know how to use various CLI commands and know the command options for tools written in go, rust and node.js as well as linux commands.
For example, the "bat" command.
I would like to be taught the options for various commands, so when I type in a command name, please output a list of pairs of options and a brief description of those options.
Output as many options as possible.
the output of the man command is also helpful.
The original command name is not required.
Be sure to start a new line for each pair you output.
Also, if you have an option that begins with "--", please output that as an option.
The output format is as follows

Output:

"Option 1", "Description of Option 1"

"Option 2", "Description of option 2"

Example
In the case of the ls command, the format is as follows.

Output:

"--all", "Do not ignore entries beginning with"

"--author", "-l to show the author of each file"


Follow the above rules to print the subcommands and option lists for the "{}" command.
"#,
                cmd
            );
            let mut items: Vec<Candidate> = Vec::new();

            match client.send_message(&content, None, Some(0.1)) {
                Ok(res) => {
                    for res in res.split('\n') {
                        if res.starts_with('"') {
                            if let Some((opt, desc)) = res.split_once(',') {
                                let opt = unquote(opt).to_string();
                                let unq_desc = unquote(desc.trim()).to_string();
                                let item = Candidate::Item(opt, unq_desc);
                                items.push(item);
                            }
                        }
                    }

                    let write_file = File::create(&completion_file_path)?;
                    let writer = BufWriter::new(write_file);
                    serde_json::to_writer(writer, &items)?;
                    items
                }
                _ => items,
            }
        };

        if items.is_empty() {
            remove_file(&completion_file_path)?;
        }
        let res = select_completion_items_simple(items, None);
        Ok(res)
    }
}

pub fn unquote(s: &str) -> String {
    let quote = s.chars().next().unwrap();

    if quote != '"' && quote != '\'' && quote != '`' {
        return s.to_string();
    }
    let s = &s[1..s.len() - 1];
    s.to_string()
}

/// Advanced completion engine that combines multiple completion strategies
///
/// This is the new integrated completion system that replaces the old AdvancedCompletion.
/// It provides enhanced functionality with JSON-based command completion, improved UI,
/// and better integration between different completion sources.
#[allow(dead_code)]
pub type AdvancedCompletion = IntegratedCompletionEngine;

/// Legacy compatibility functions for AdvancedCompletion
impl IntegratedCompletionEngine {
    /// Get completion for a specific command (legacy compatibility)
    #[allow(dead_code)]
    pub fn complete_command(
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

        let enhanced_candidates = self.complete(&input, input.len(), current_dir, 20);

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
                    priority: 50,
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
                    priority: if is_dir { 50 } else { 40 },
                    source: CandidateSource::Context,
                },
                Candidate::Path(path) => EnhancedCandidate {
                    text: path,
                    description: None,
                    candidate_type: CandidateType::File,
                    priority: 40,
                    source: CandidateSource::Context,
                },
                Candidate::Basic(text) => EnhancedCandidate {
                    text,
                    description: None,
                    candidate_type: CandidateType::Generic,
                    priority: 30,
                    source: CandidateSource::Context,
                },
                Candidate::Command { name, description } => EnhancedCandidate {
                    text: name,
                    description: Some(description),
                    candidate_type: CandidateType::SubCommand,
                    priority: 80,
                    source: CandidateSource::Context,
                },
                Candidate::Option { name, description } => EnhancedCandidate {
                    text: name,
                    description: Some(description),
                    candidate_type: CandidateType::LongOption,
                    priority: 70,
                    source: CandidateSource::Context,
                },
                Candidate::GitBranch { name, .. } => EnhancedCandidate {
                    text: name,
                    description: None,
                    candidate_type: CandidateType::Generic,
                    priority: 60,
                    source: CandidateSource::Context,
                },
                Candidate::NpmScript { name } => EnhancedCandidate {
                    text: name,
                    description: None,
                    candidate_type: CandidateType::Generic,
                    priority: 50,
                    source: CandidateSource::Context,
                },
                Candidate::History { command, .. } => EnhancedCandidate {
                    text: command,
                    description: None,
                    candidate_type: CandidateType::Generic,
                    priority: 40,
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
    #[allow(dead_code)]
    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
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
    //         // 環境依存のため、git関連のディレクトリが存在することを確認
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
    //             .with_message_template("他に{}個のアイテムがあります")
    //             .with_item_count_display(false);
    //
    //         assert_eq!(config.max_items, 10);
    //         assert_eq!(
    //             config.more_items_message_template,
    //             "他に{}個のアイテムがあります"
    //         );
    //         assert!(!config.show_item_count);
    //
    //         // Test message formatting
    //         let message = config.format_more_items_message(25);
    //         assert_eq!(message, "他に25個のアイテムがあります");
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
    //         // ASCII文字のテスト
    //         assert_eq!(unicode_display_width("hello"), 5);
    //
    //         // 日本語文字のテスト（全角文字は2文字幅）
    //         assert_eq!(unicode_display_width("こんにちは"), 10);
    //
    //         // 絵文字のテスト
    //         assert_eq!(unicode_display_width("🐕"), 2);
    //         assert_eq!(unicode_display_width("⚡"), 2);
    //         assert_eq!(unicode_display_width("📁"), 2);
    //
    //         // 混在のテスト
    //         assert_eq!(unicode_display_width("hello世界🐕"), 5 + 4 + 2); // 11
    //     }

    //     #[test]
    //     fn test_truncate_to_width() {
    //         init();
    //
    //         // ASCII文字の切り詰めテスト
    //         assert_eq!(truncate_to_width("hello_world", 5), "hell…");
    //         assert_eq!(truncate_to_width("hello", 10), "hello");
    //
    //         // 日本語文字の切り詰めテスト
    //         assert_eq!(truncate_to_width("こんにちは", 6), "こん…"); // 4文字幅 + 1文字幅（…）= 5文字幅
    //
    //         // 絵文字の切り詰めテスト
    //         assert_eq!(truncate_to_width("🐕🚀⚡", 4), "🐕…"); // 2文字幅 + 1文字幅（…）= 3文字幅
    //
    //         // 混在の切り詰めテスト
    //         assert_eq!(truncate_to_width("hello世界", 8), "hello世…"); // 5文字幅 + 2文字幅 + 1文字幅（…）= 8文字幅
    //     }

    //     #[test]
    //     fn test_candidate_formatted_display_unicode() {
    //         init();
    //
    //         // 日本語ファイル名のテスト
    //         let candidate = Candidate::File {
    //             path: "日本語ファイル.txt".to_string(),
    //             is_dir: false,
    //         };
    //
    //         let formatted = candidate.get_formatted_display(20);
    //         let display_width = unicode_display_width(&formatted);
    //
    //         // 表示幅が指定した幅以下であることを確認
    //         assert!(display_width <= 20);
    //
    //         // 絵文字が含まれていることを確認
    //         assert!(formatted.contains("📄"));
    //     }
}
