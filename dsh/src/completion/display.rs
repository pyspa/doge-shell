#![allow(dead_code)]
use anyhow::Result;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{cursor, execute, queue};
use serde::{Deserialize, Serialize};
use std::io::{Write, stdout};
use tracing::debug;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// Completion display configuration
const MAX_COMPLETION_ITEMS: usize = 30;

fn unicode_display_width(s: &str) -> usize {
    s.width()
}

/// Truncate a Unicode string to fit within the specified display width
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

/// Completion candidate display settings
#[derive(Debug, Clone)]
pub struct DisplayConfig {
    /// Maximum display rows
    pub max_rows: usize,
    /// Maximum display columns
    pub max_columns: usize,
    /// Whether to show descriptions
    pub show_descriptions: bool,
    /// Whether to show icons
    pub show_icons: bool,
    /// Whether to use color coding
    pub use_colors: bool,
    /// Maximum characters per line
    pub max_width_per_item: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            max_rows: 10,
            max_columns: 3,
            show_descriptions: true,
            show_icons: true,
            use_colors: true,
            max_width_per_item: 40,
        }
    }
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

    pub fn format_more_items_message(&self, remaining_count: usize) -> String {
        if self.more_items_message_template.contains("{}") {
            self.more_items_message_template
                .replace("{}", &remaining_count.to_string())
        } else {
            format!("{} ({})", self.more_items_message_template, remaining_count)
        }
    }
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub fn new(candidates: Vec<Candidate>, prompt_text: String, input_text: String) -> Self {
        Self::new_with_config(
            candidates,
            &prompt_text,
            &input_text,
            CompletionConfig::default(),
        )
    }

    pub fn new_with_config(
        mut candidates: Vec<Candidate>,
        prompt_text: &str,
        input_text: &str,
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
            .map(|c| {
                let name_width = unicode_display_width(c.get_display_name());
                let type_char_width = c.get_type_char().width().unwrap_or(2); // Most emojis are 2 chars wide
                name_width + type_char_width + 2 // type_char + " " + name + " "
            })
            .max()
            .unwrap_or(10);

        // Limit column width to prevent extremely wide columns and ensure no wrapping
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
            prompt_text: prompt_text.to_string(),
            input_text: input_text.to_string(),
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

        // Calculate the total display width of prompt + input for cursor positioning
        // Move cursor to start of completion area
        execute!(stdout, cursor::MoveToNextLine(1))?;

        // Get terminal width to prevent wrapping
        let (terminal_width, _) = crossterm::terminal::size()?;
        let terminal_width = terminal_width as usize;

        for row in 0..self.total_rows {
            let mut current_line_width = 0;

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

                // Calculate the width this item will take
                let formatted = if is_message_item {
                    candidate.get_display_name().to_string()
                } else {
                    candidate.get_formatted_display(self.column_width)
                };

                let item_width = 1 + unicode_display_width(&formatted); // ">" or " " + formatted
                let spacing_width =
                    if col < self.items_per_row - 1 && index + 1 < self.candidates.len() {
                        1
                    } else {
                        0
                    };
                let total_item_width = item_width + spacing_width;

                // Check if adding this item would cause wrapping
                if current_line_width + total_item_width > terminal_width {
                    break; // Skip this item to prevent wrapping
                }

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
                        '⚡' => queue!(stdout, SetForegroundColor(Color::Green))?, // Command - lightning bolt
                        '📁' => queue!(stdout, SetForegroundColor(Color::Blue))?, // Directory - folder
                        '📄' => queue!(stdout, SetForegroundColor(Color::White))?, // File - document
                        '⚙' => queue!(stdout, SetForegroundColor(Color::Yellow))?, // Option - gear
                        '🔹' => queue!(stdout, SetForegroundColor(Color::White))?, // Basic - small blue diamond
                        '🌿' => queue!(stdout, SetForegroundColor(Color::Green))?, // Git branch - herb/branch
                        '📜' => queue!(stdout, SetForegroundColor(Color::Cyan))?, // Script - scroll
                        '🕒' => queue!(stdout, SetForegroundColor(Color::Magenta))?, // History - clock
                        _ => queue!(stdout, SetForegroundColor(Color::White))?,
                    }
                }

                queue!(stdout, Print(formatted))?;
                queue!(stdout, ResetColor)?;

                current_line_width += item_width;

                // Add spacing between columns
                if col < self.items_per_row - 1 && index + 1 < self.candidates.len() {
                    queue!(stdout, Print(" "))?;
                    current_line_width += spacing_width;
                }
            }
            if row < self.total_rows - 1 {
                queue!(stdout, cursor::MoveToNextLine(1))?;
            }
        }

        // Move cursor back to the end of input line (but keep it hidden)
        if let (Some(start_row), Some(start_col)) = (self.display_start_row, self.display_start_col)
        {
            let prompt_width = unicode_display_width(&self.prompt_text);
            let input_width = unicode_display_width(&self.input_text);
            let input_end_col = start_col + prompt_width as u16 + input_width as u16;
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
            let prompt_width = unicode_display_width(&self.prompt_text);
            let input_width = unicode_display_width(&self.input_text);
            let input_end_col = start_col + prompt_width as u16 + input_width as u16;
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

/// Simple display function (for compatibility with existing systems)
// pub fn display_candidates_simple(candidates: &[EnhancedCandidate]) -> IoResult<()> {
//     let display = CompletionDisplay::with_default_config();
//     display.display_candidates(candidates)
// }

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
    GitBranch {
        name: String,
        is_current: bool,
    },
    File {
        path: String,
        is_dir: bool,
    },
    History {
        command: String,
        frequency: u32,
        last_used: i64,
    },
}

impl Candidate {
    /// Get the type character for display
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
            Candidate::History { command, .. } => command,
        }
    }

    /// Get formatted display string with type character
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::integrated::{CandidateSource, CandidateType, EnhancedCandidate};

    fn create_test_candidate(text: &str, candidate_type: CandidateType) -> EnhancedCandidate {
        EnhancedCandidate {
            text: text.to_string(),
            description: Some(format!("Description for {}", text)),
            candidate_type,
            priority: 100,
            source: CandidateSource::Command,
        }
    }

    #[test]
    fn test_display_config_default() {
        let config = DisplayConfig::default();
        assert_eq!(config.max_rows, 10);
        assert_eq!(config.max_columns, 3);
        assert!(config.show_descriptions);
        assert!(config.show_icons);
        assert!(config.use_colors);
    }
}
