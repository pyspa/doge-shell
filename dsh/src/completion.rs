use crate::dirs::is_executable;
use crate::environment::get_data_file;
use crate::input::Input;
use crate::lisp::Value;
use crate::repl::Repl;
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, read};
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{cursor, execute, queue};
use dsh_frecency::ItemStats;
use regex::Regex;
use serde::{Deserialize, Serialize};
use skim::prelude::*;
use skim::{Skim, SkimItemReceiver, SkimItemSender};
use std::borrow::Cow;
use std::fs::{File, create_dir_all, read_dir, remove_file};
use std::io::{BufReader, BufWriter, Write, stdout};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::{process::Command, sync::Arc};
use tracing::debug;

// Completion display configuration
const MAX_COMPLETION_ITEMS: usize = 30;

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
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_items(mut self, max_items: usize) -> Self {
        self.max_items = max_items;
        self
    }

    pub fn with_message_template<S: Into<String>>(mut self, template: S) -> Self {
        self.more_items_message_template = template.into();
        self
    }

    pub fn with_item_count_display(mut self, show: bool) -> Self {
        self.show_item_count = show;
        self
    }

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

#[derive(Debug)]
pub struct CompletionDisplay {
    candidates: Vec<Candidate>,
    selected_index: usize,
    #[allow(dead_code)]
    terminal_width: usize,
    column_width: usize,
    items_per_row: usize,
    total_rows: usize,
    display_start_row: Option<u16>,
    display_start_col: Option<u16>,
    prompt_text: String,
    input_text: String,
    cursor_hidden: bool,
    config: CompletionConfig,
    has_more_items: bool,
    total_items_count: usize,
}

impl Drop for CompletionDisplay {
    fn drop(&mut self) {
        // Ensure cursor is shown when CompletionDisplay is dropped
        if self.cursor_hidden {
            let _ = execute!(stdout(), cursor::Show);
        }
    }
}

impl CompletionDisplay {
    pub fn new(candidates: Vec<Candidate>, prompt_text: String, input_text: String) -> Self {
        Self::new_with_config(
            candidates,
            prompt_text,
            input_text,
            CompletionConfig::default(),
        )
    }

    pub fn new_with_config(
        mut candidates: Vec<Candidate>,
        prompt_text: String,
        input_text: String,
        config: CompletionConfig,
    ) -> Self {
        let terminal_width = term_size::dimensions().map(|(w, _)| w).unwrap_or(80);
        let total_items_count = candidates.len();
        let has_more_items = total_items_count > config.max_items;

        // Limit candidates to max_items
        if has_more_items {
            candidates.truncate(config.max_items);

            // Add a message candidate to show there are more items
            if config.show_item_count {
                let remaining_count = total_items_count - config.max_items;
                let message = config.format_more_items_message(remaining_count);
                candidates.push(Candidate::Basic(format!("📋 {}", message)));
            }
        }

        // Calculate the maximum display width needed
        let max_display_width = candidates
            .iter()
            .map(|c| c.get_display_name().len() + 3) // "C " + name + " "
            .max()
            .unwrap_or(10);

        // Limit column width to prevent extremely wide columns
        let column_width = std::cmp::min(max_display_width, terminal_width / 3);

        let items_per_row = if column_width > 0 {
            std::cmp::max(1, terminal_width / column_width)
        } else {
            1
        };

        let total_rows = candidates.len().div_ceil(items_per_row);

        CompletionDisplay {
            candidates,
            selected_index: 0,
            terminal_width,
            column_width,
            items_per_row,
            total_rows,
            display_start_row: None,
            display_start_col: None,
            prompt_text,
            input_text,
            cursor_hidden: false,
            config,
            has_more_items,
            total_items_count,
        }
    }

    /// Ensure there's enough space below the cursor for completion display
    fn ensure_display_space(&mut self) -> Result<()> {
        let mut stdout = stdout();

        // Get current terminal size and cursor position
        let terminal_size = crossterm::terminal::size()?;
        let terminal_height = terminal_size.1;

        let current_row = if let Some(row) = self.display_start_row {
            row
        } else if let Ok((_, row)) = cursor::position() {
            row
        } else {
            return Ok(()); // Can't determine position, skip space creation
        };

        let available_rows = terminal_height.saturating_sub(current_row + 1);
        let needed_rows = self.total_rows as u16;

        debug!(
            "Space check - Terminal height: {}, current row: {}, available: {}, needed: {}",
            terminal_height, current_row, available_rows, needed_rows
        );

        // If we don't have enough space, create it
        if needed_rows > available_rows {
            let rows_to_create = needed_rows - available_rows;
            debug!(
                "Creating {} rows of space for completion display",
                rows_to_create
            );

            // Save current cursor position
            let (original_col, original_row) = cursor::position().unwrap_or((0, current_row));

            // Create space by moving to the bottom and adding newlines
            // This will cause the terminal to scroll up naturally
            execute!(stdout, cursor::MoveTo(0, terminal_height - 1))?;
            for _ in 0..rows_to_create {
                execute!(stdout, Print("\n"))?;
            }

            // Update our recorded position since content has shifted up
            let new_row = original_row.saturating_sub(rows_to_create);

            self.display_start_row = Some(new_row);
            debug!("Updated display start position to row: {}", new_row);

            // Move cursor back to the updated position
            execute!(stdout, cursor::MoveTo(original_col, new_row))?;
        }

        Ok(())
    }

    pub fn move_up(&mut self) {
        if self.selected_index >= self.items_per_row {
            self.selected_index -= self.items_per_row;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected_index + self.items_per_row < self.candidates.len() {
            self.selected_index += self.items_per_row;
        }
    }

    pub fn move_left(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    pub fn move_right(&mut self) {
        if self.selected_index + 1 < self.candidates.len() {
            self.selected_index += 1;
        }
    }

    pub fn get_selected(&self) -> Option<&Candidate> {
        if let Some(candidate) = self.candidates.get(self.selected_index) {
            // Don't return message items as selectable
            if self.has_more_items
                && self.selected_index == self.candidates.len() - 1
                && candidate.get_display_name().starts_with("📋")
            {
                return None;
            }
            Some(candidate)
        } else {
            None
        }
    }

    pub fn display(&mut self) -> Result<()> {
        let mut stdout = stdout();

        // Hide cursor during completion display (only once)
        if !self.cursor_hidden {
            execute!(stdout, cursor::Hide)?;
            self.cursor_hidden = true;
        }

        // Record current cursor position before displaying
        if self.display_start_row.is_none() {
            if let Ok((col, row)) = cursor::position() {
                debug!("Recording display start position: col={}, row={}", col, row);
                self.display_start_row = Some(row);
                self.display_start_col = Some(col);
            } else {
                debug!("Failed to get cursor position");
            }
        }

        // Ensure we have enough space for the completion display
        self.ensure_display_space()?;

        // Clear the current line and redraw prompt + input
        execute!(
            stdout,
            cursor::MoveToColumn(0),
            Clear(ClearType::CurrentLine)
        )?;
        queue!(stdout, Print(&self.prompt_text))?;
        queue!(stdout, Print(&self.input_text))?;

        // Move cursor to start of completion area
        execute!(stdout, cursor::MoveToNextLine(1))?;

        for row in 0..self.total_rows {
            for col in 0..self.items_per_row {
                let index = row * self.items_per_row + col;
                if index >= self.candidates.len() {
                    break;
                }

                let candidate = &self.candidates[index];
                let is_selected = index == self.selected_index;
                let is_message_item = self.has_more_items
                    && index == self.candidates.len() - 1
                    && candidate.get_display_name().starts_with("📋");

                // Display with type character and proper formatting
                if is_selected {
                    queue!(stdout, SetForegroundColor(Color::Yellow))?;
                    queue!(stdout, Print(">"))?;
                } else {
                    queue!(stdout, Print(" "))?;
                }

                // Add type-specific coloring
                if is_message_item {
                    queue!(stdout, SetForegroundColor(Color::DarkGrey))?;
                } else {
                    match candidate.get_type_char() {
                        'C' => queue!(stdout, SetForegroundColor(Color::Green))?, // Command
                        'D' => queue!(stdout, SetForegroundColor(Color::Blue))?,  // Directory
                        'F' => queue!(stdout, SetForegroundColor(Color::White))?, // File
                        'O' => queue!(stdout, SetForegroundColor(Color::Yellow))?, // Option
                        'P' => queue!(stdout, SetForegroundColor(Color::Cyan))?,  // Path
                        'B' => queue!(stdout, SetForegroundColor(Color::White))?, // Basic
                        _ => queue!(stdout, SetForegroundColor(Color::White))?,
                    }
                }

                let formatted = if is_message_item {
                    // Display message items without type character formatting
                    candidate.get_display_name()
                } else {
                    candidate.get_formatted_display(self.column_width)
                };

                queue!(stdout, Print(formatted))?;
                queue!(stdout, ResetColor)?;

                // Add spacing between columns
                if col < self.items_per_row - 1 && index + 1 < self.candidates.len() {
                    queue!(stdout, Print(" "))?;
                }
            }
            if row < self.total_rows - 1 {
                queue!(stdout, cursor::MoveToNextLine(1))?;
            }
        }

        // Move cursor back to the end of input line (but keep it hidden)
        if let (Some(start_row), Some(start_col)) = (self.display_start_row, self.display_start_col)
        {
            let input_end_col = start_col + self.input_text.chars().count() as u16;
            execute!(stdout, cursor::MoveTo(input_end_col, start_row))?;
        }

        stdout.flush()?;
        debug!(
            "Displayed {} candidates in {} rows (total items: {}, has_more: {})",
            self.candidates.len(),
            self.total_rows,
            self.total_items_count,
            self.has_more_items
        );
        Ok(())
    }

    pub fn clear_display(&mut self) -> Result<()> {
        let mut stdout = stdout();

        debug!("Clearing completion display with {} rows", self.total_rows);

        // If we have recorded position, move back to it first
        if let (Some(start_row), Some(start_col)) = (self.display_start_row, self.display_start_col)
        {
            debug!(
                "Using recorded position: col={}, row={}",
                start_col, start_row
            );

            // Move to the start position
            execute!(stdout, cursor::MoveTo(start_col, start_row))?;

            // Clear from the start position down to the end of completion area
            // Clear the current line first
            execute!(stdout, Clear(ClearType::CurrentLine))?;

            // Then clear each subsequent line
            for i in 0..self.total_rows {
                execute!(
                    stdout,
                    cursor::MoveToNextLine(1),
                    Clear(ClearType::CurrentLine)
                )?;
                debug!("Cleared completion line {}", i + 1);
            }

            // Move back to the original position and redraw prompt + input
            execute!(stdout, cursor::MoveTo(start_col, start_row))?;
            queue!(stdout, Print(&self.prompt_text))?;
            queue!(stdout, Print(&self.input_text))?;

            // Position cursor at the end of input
            let input_end_col = start_col + self.input_text.chars().count() as u16;
            execute!(stdout, cursor::MoveTo(input_end_col, start_row))?;
        } else {
            debug!("Using fallback clear method");

            // Fallback: clear using the old method with additional safety
            // First, try to clear the current line
            execute!(stdout, Clear(ClearType::CurrentLine))?;

            // Then move up and clear each line
            for i in 0..self.total_rows {
                execute!(
                    stdout,
                    cursor::MoveToPreviousLine(1),
                    Clear(ClearType::CurrentLine)
                )?;
                debug!("Cleared line {} (moving up)", i + 1);
            }

            // Redraw prompt + input
            queue!(stdout, Print(&self.prompt_text))?;
            queue!(stdout, Print(&self.input_text))?;
        }

        // Show cursor again after clearing completion display
        if self.cursor_hidden {
            execute!(stdout, cursor::Show)?;
            self.cursor_hidden = false;
        }

        // Reset the recorded position
        self.display_start_row = None;
        self.display_start_col = None;

        stdout.flush()?;
        debug!("Completion display cleared successfully");
        Ok(())
    }
}

#[derive(Debug)]
pub struct Completion {
    pub input: Option<String>,
    completions: Vec<ItemStats>,
    current_index: usize,
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
        let item = ItemStats::new(input, 0.0, 0.0);

        self.input = if input.is_empty() {
            None
        } else {
            Some(input.to_string())
        };
        self.completions = comps;
        self.completions.insert(0, item);
        self.current_index = 0;
    }

    pub fn backward(&mut self) -> Option<ItemStats> {
        if self.completions.is_empty() {
            return None;
        }

        if self.completions.len() - 1 > self.current_index {
            self.current_index += 1;
            Some(self.completions[self.current_index].clone())
        } else {
            None
        }
    }

    pub fn forward(&mut self) -> Option<ItemStats> {
        if self.current_index > 0 {
            self.current_index -= 1;
            Some(self.completions[self.current_index].clone())
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
}

impl Candidate {
    /// Get the type character for display
    pub fn get_type_char(&self) -> char {
        match self {
            Candidate::Item(_, desc) => {
                if desc.contains("command") {
                    'C'
                } else if desc.contains("file") {
                    'F'
                } else if desc.contains("directory") {
                    'D'
                } else {
                    'O' // Option or other
                }
            }
            Candidate::Path(path) => {
                if path.ends_with('/') {
                    'D' // Directory
                } else {
                    'F' // File
                }
            }
            Candidate::Basic(_) => 'B', // Basic
        }
    }

    /// Get the display name (without description)
    pub fn get_display_name(&self) -> String {
        match self {
            Candidate::Item(name, _) => name.clone(),
            Candidate::Path(path) => path.clone(),
            Candidate::Basic(basic) => basic.clone(),
        }
    }

    /// Get formatted display string with type character
    pub fn get_formatted_display(&self, width: usize) -> String {
        let type_char = self.get_type_char();
        let name = self.get_display_name();

        // Truncate name if too long
        let max_name_width = width.saturating_sub(3); // "C " + " "
        let display_name = if name.len() > max_name_width {
            format!("{}…", &name[..max_name_width.saturating_sub(1)])
        } else {
            name
        };

        format!(
            "{} {:<width$}",
            type_char,
            display_name,
            width = max_name_width
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

    let home = dirs::home_dir().unwrap();
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
    prompt_text: String,
    input_text: String,
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
    prompt_text: String,
    input_text: String,
    config: CompletionConfig,
) -> Option<String> {
    if items.is_empty() {
        return None;
    }

    let mut display =
        CompletionDisplay::new_with_config(items.clone(), prompt_text, input_text, config);

    // Show initial display (cursor will be hidden in display method)
    if display.display().is_err() {
        // If display fails, make sure cursor is shown
        let _ = execute!(stdout(), cursor::Show);
        return None;
    }

    loop {
        if let Ok(Event::Key(KeyEvent {
            code, modifiers, ..
        })) = read()
        {
            match (code, modifiers) {
                (KeyCode::Up, KeyModifiers::NONE) => {
                    let _ = display.clear_display();
                    display.move_up();
                    let _ = display.display();
                }
                (KeyCode::Down, KeyModifiers::NONE) => {
                    let _ = display.clear_display();
                    display.move_down();
                    let _ = display.display();
                }
                (KeyCode::Left, KeyModifiers::NONE) => {
                    let _ = display.clear_display();
                    display.move_left();
                    let _ = display.display();
                }
                (KeyCode::Right, KeyModifiers::NONE) => {
                    let _ = display.clear_display();
                    display.move_right();
                    let _ = display.display();
                }
                (KeyCode::Enter, KeyModifiers::NONE) => {
                    let _ = display.clear_display();
                    if let Some(selected_candidate) = display.get_selected() {
                        return Some(selected_candidate.output().to_string());
                    }
                    return None;
                }
                (KeyCode::Esc, KeyModifiers::NONE)
                | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    let _ = display.clear_display();
                    return None;
                }
                _ => {}
            }
        }
    }
}

// Backward compatibility function
pub fn select_completion_items_simple(
    items: Vec<Candidate>,
    query: Option<&str>,
) -> Option<String> {
    let (prompt_text, input_text) = get_prompt_and_input_for_completion();
    select_completion_items(items, query, prompt_text, input_text)
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

fn completion_from_current(_input: &Input, repl: &Repl, query: Option<&str>) -> Option<String> {
    let lisp_engine = Rc::clone(&repl.shell.lisp_engine);
    let environment = Arc::clone(&lisp_engine.borrow().shell_env);

    // 2 . try completion
    if let Some(query_str) = query {
        // check path
        let current = std::env::current_dir().expect("fail get current_dir");

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
            (current.as_path(), "", false)
        };

        let canonical_path = if let Ok(path) = path.canonicalize() {
            path
        } else {
            std::env::current_dir().expect("fail get current_dir")
        };
        let path_str = canonical_path.display().to_string();

        // path
        let mut items = get_file_completions(path_str.as_str(), path.to_str().unwrap());
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
    prompt_text: String,
    input_text: String,
) -> Option<String> {
    let res = completion_from_lisp_with_prompt(
        input,
        repl,
        query,
        prompt_text.clone(),
        input_text.clone(),
    );
    if res.is_some() {
        return res;
    }
    let res = completion_from_current_with_prompt(
        input,
        repl,
        query,
        prompt_text.clone(),
        input_text.clone(),
    );
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

// Backward compatibility function
pub fn input_completion_simple(input: &Input, repl: &Repl, query: Option<&str>) -> Option<String> {
    let (prompt_text, input_text) = get_prompt_and_input_for_completion();
    input_completion(input, repl, query, prompt_text, input_text)
}

fn completion_from_lisp_with_prompt(
    input: &Input,
    repl: &Repl,
    query: Option<&str>,
    prompt_text: String,
    input_text: String,
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
    prompt_text: String,
    input_text: String,
) -> Option<String> {
    let lisp_engine = Rc::clone(&repl.shell.lisp_engine);
    let environment = Arc::clone(&lisp_engine.borrow().shell_env);

    // 2 . try completion
    if let Some(query_str) = query {
        // check path
        let current = std::env::current_dir().expect("fail get current_dir");

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
            (current.as_path(), "", false)
        };

        let canonical_path = if let Ok(path) = path.canonicalize() {
            path
        } else {
            std::env::current_dir().expect("fail get current_dir")
        };
        let path_str = canonical_path.display().to_string();

        // path
        let mut items = get_file_completions(path_str.as_str(), path.to_str().unwrap());
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

fn replace_space(s: &str) -> String {
    let re = Regex::new(r"\s+").unwrap();
    re.replace_all(s, "_").to_string()
}

#[derive(Debug)]
pub struct ChatGPTCompletion {
    api_key: String,
    pub store_path: PathBuf,
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

#[cfg(test)]
mod tests {

    use super::*;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn test_completion() -> Result<()> {
        init();
        let p = path_completion_prefix(".")?;
        assert_eq!(None, p);

        let p = path_completion_prefix("./")?;
        assert_eq!(None, p);

        let p = path_completion_prefix("./sr")?;
        assert_eq!(Some("./src/".to_string()), p);

        let p = path_completion_prefix("sr")?;
        assert_eq!(Some("src/".to_string()), p);

        // let p = path_completion_first("src/b")?;
        // assert_eq!(Some("src/builtin/".to_string()), p);

        let p = path_completion_prefix("/")?;
        assert_eq!(None, p);

        let p = path_completion_prefix("/s")?;
        assert_eq!(Some("/sbin/".to_string()), p);

        let p = path_completion_prefix("/usr/b")?;
        assert_eq!(Some("/usr/bin/".to_string()), p);

        let p = path_completion_prefix("~/.lo")?;
        assert_eq!(Some("~/.local/".to_string()), p);

        let p = path_completion_prefix("~/.config/gi")?;
        // 環境依存のため、git関連のディレクトリが存在することを確認
        assert!(p.is_some());
        assert!(p.unwrap().starts_with("~/.config/git"));

        Ok(())
    }

    #[test]
    #[ignore]
    fn test_select_item() {
        init();
        let mut items: Vec<Candidate> = Vec::new();
        // items.push();
        items.push(Candidate::Basic("test1".to_string()));
        items.push(Candidate::Basic("test2".to_string()));

        let a = select_completion_items_simple(items, Some("test"));
        assert_eq!("test1", a.unwrap());
    }

    #[test]
    fn test_completion_config() {
        init();

        // Test default config
        let config = CompletionConfig::default();
        assert_eq!(config.max_items, 30);
        assert_eq!(
            config.more_items_message_template,
            "...and {} more items available"
        );
        assert!(config.show_item_count);

        // Test custom config
        let config = CompletionConfig::new()
            .with_max_items(10)
            .with_message_template("他に{}個のアイテムがあります")
            .with_item_count_display(false);

        assert_eq!(config.max_items, 10);
        assert_eq!(
            config.more_items_message_template,
            "他に{}個のアイテムがあります"
        );
        assert!(!config.show_item_count);

        // Test message formatting
        let message = config.format_more_items_message(25);
        assert_eq!(message, "他に25個のアイテムがあります");
    }

    #[test]
    fn test_completion_display_with_limit() {
        init();

        // Create more than 30 candidates
        let mut candidates = Vec::new();
        for i in 0..50 {
            candidates.push(Candidate::Basic(format!("item_{:02}", i)));
        }

        let config = CompletionConfig::default();
        let comp_display = CompletionDisplay::new_with_config(
            candidates,
            "$ ".to_string(),
            "test".to_string(),
            config,
        );

        // Should have 30 items + 1 message item
        assert_eq!(comp_display.candidates.len(), 31);
        assert!(comp_display.has_more_items);
        assert_eq!(comp_display.total_items_count, 50);

        // Last item should be the message
        let last_item = &comp_display.candidates[30];
        assert!(last_item.get_display_name().starts_with("📋"));
        assert!(last_item.get_display_name().contains("20 more items"));
    }

    #[test]
    fn test_completion_display_no_limit() {
        init();

        // Create less than 30 candidates
        let mut candidates = Vec::new();
        for i in 0..10 {
            candidates.push(Candidate::Basic(format!("item_{:02}", i)));
        }

        let config = CompletionConfig::default();
        let comp_display = CompletionDisplay::new_with_config(
            candidates,
            "$ ".to_string(),
            "test".to_string(),
            config,
        );

        // Should have exactly 10 items, no message
        assert_eq!(comp_display.candidates.len(), 10);
        assert!(!comp_display.has_more_items);
        assert_eq!(comp_display.total_items_count, 10);
    }

    #[test]
    fn test_completion_display_space_calculation() {
        init();

        // Create many candidates to test space requirements
        let mut candidates = Vec::new();
        for i in 0..100 {
            candidates.push(Candidate::Basic(format!("item_{:03}", i)));
        }

        let config = CompletionConfig::default().with_max_items(50);
        let comp_display = CompletionDisplay::new_with_config(
            candidates,
            "$ ".to_string(),
            "test_command".to_string(),
            config,
        );

        // Should limit to 50 items + 1 message
        assert_eq!(comp_display.candidates.len(), 51);
        assert!(comp_display.has_more_items);
        assert_eq!(comp_display.total_items_count, 100);

        // Check that total_rows is calculated correctly
        let expected_rows = comp_display
            .candidates
            .len()
            .div_ceil(comp_display.items_per_row);
        assert_eq!(comp_display.total_rows, expected_rows);

        debug!(
            "Display has {} rows with {} items per row",
            comp_display.total_rows, comp_display.items_per_row
        );
    }

    #[test]
    fn test_completion_display_small_terminal() {
        init();

        // Test with a scenario that would require space creation
        let mut candidates = Vec::new();
        for i in 0..20 {
            candidates.push(Candidate::Basic(format!("command_{}", i)));
        }

        let config = CompletionConfig::default();
        let comp_display = CompletionDisplay::new_with_config(
            candidates,
            "user@host:~/project$ ".to_string(),
            "git ".to_string(),
            config,
        );

        // Verify the display is properly configured
        assert_eq!(comp_display.candidates.len(), 20);
        assert!(!comp_display.has_more_items);
        assert!(comp_display.total_rows > 0);

        // The ensure_display_space method should handle space creation
        // This is mainly tested through integration testing
    }
}
