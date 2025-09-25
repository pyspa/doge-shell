#![allow(dead_code)]
use super::ui::CompletionUi;
use anyhow::Result;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{cursor, execute, queue};
use serde::{Deserialize, Serialize};
use skim::prelude::SkimItem;
use std::io::{Write, stdout};
use tracing::debug;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, Copy)]
enum DisplayMode {
    Full,
    SelectionOnly,
}

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
        // Use crossterm for more reliable terminal size detection
        let terminal_width = crossterm::terminal::size()
            .map(|(w, _)| w as usize)
            .unwrap_or(80);

        debug!("Terminal width detected: {}", terminal_width);

        let total_items_count = candidates.len();
        let has_more_items = total_items_count > config.max_items;

        // Limit candidates to max_items
        if has_more_items {
            candidates.truncate(config.max_items);

            // Add a message candidate to show there are more items
            if config.show_item_count {
                let remaining_count = total_items_count - config.max_items;
                let message = config.format_more_items_message(remaining_count);
                candidates.push(Candidate::Basic(format!("📋 {message}")));
            }
        }

        // Calculate the maximum display width needed for each candidate
        let max_display_width = candidates
            .iter()
            .map(|c| {
                let name_width = unicode_display_width(c.get_display_name());
                let type_char_width = c.get_type_char().width().unwrap_or(2); // Most emojis are 2 chars wide
                name_width + type_char_width + 1 // type_char + " " + name
            })
            .max()
            .unwrap_or(10);

        debug!("Max display width needed: {}", max_display_width);

        // Reserve space for selection indicator ("> " or "  ") and inter-column spacing
        let selection_indicator_width = 1; // ">" or " "
        let inter_column_spacing = 2; // Space between columns

        // Calculate effective column width including all necessary spacing
        let effective_column_width = max_display_width + selection_indicator_width;

        // Calculate how many items can fit per row, accounting for spacing between columns
        let items_per_row = if effective_column_width > 0 {
            let available_width = terminal_width.saturating_sub(4); // Reserve 4 chars margin for safety
            let width_per_item = effective_column_width + inter_column_spacing;

            // Calculate maximum items that can fit
            let max_items = if width_per_item > 0 {
                std::cmp::max(1, available_width / width_per_item)
            } else {
                1
            };

            // Ensure at least 1 item per row, but don't exceed what can actually fit
            std::cmp::min(max_items, candidates.len())
        } else {
            1
        };

        // Recalculate column width based on actual items per row to ensure proper fit
        let column_width = if items_per_row > 0 {
            let available_width = terminal_width.saturating_sub(4); // Reserve margin
            let total_spacing = (items_per_row.saturating_sub(1)) * inter_column_spacing;
            let width_for_content = available_width.saturating_sub(total_spacing);
            width_for_content / items_per_row
        } else {
            terminal_width.saturating_sub(4)
        };

        let total_rows = candidates.len().div_ceil(items_per_row);

        debug!(
            "Display layout: terminal_width={}, column_width={}, items_per_row={}, total_rows={}",
            terminal_width, column_width, items_per_row, total_rows
        );

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
        self.display_with_mode(DisplayMode::Full)
    }

    pub fn update_selection(&mut self) -> Result<()> {
        self.display_with_mode(DisplayMode::SelectionOnly)
    }

    fn display_with_mode(&mut self, mode: DisplayMode) -> Result<()> {
        let mut stdout = stdout();

        // Hide cursor during completion display (only once)
        if !self.cursor_hidden {
            execute!(stdout, cursor::Hide)?;
            self.cursor_hidden = true;
        }

        match mode {
            DisplayMode::Full => {
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
            }
            DisplayMode::SelectionOnly => {
                // For selection-only updates, move to the completion area start
                if let Some(start_row) = self.display_start_row {
                    execute!(stdout, cursor::MoveTo(0, start_row + 1))?;
                } else {
                    // Fallback to full display if we don't have position info
                    return self.display_with_mode(DisplayMode::Full);
                }
            }
        }

        // Handle terminal resizing only for full display mode
        if matches!(mode, DisplayMode::Full) {
            // Get current terminal width to handle dynamic resizing
            let (current_terminal_width, _) = crossterm::terminal::size()?;
            let current_terminal_width = current_terminal_width as usize;

            debug!(
                "Current terminal width: {}, stored width: {}",
                current_terminal_width, self.terminal_width
            );

            // Update our calculations if terminal was resized
            if current_terminal_width != self.terminal_width {
                debug!("Terminal was resized, recalculating layout");
                self.terminal_width = current_terminal_width;

                // Recalculate items per row based on new terminal width
                let available_width = self.terminal_width.saturating_sub(4); // Reserve margin
                let inter_column_spacing = 2;
                let selection_indicator_width = 1;

                let max_display_width = self
                    .candidates
                    .iter()
                    .map(|c| {
                        let name_width = unicode_display_width(c.get_display_name());
                        let type_char_width = c.get_type_char().width().unwrap_or(2);
                        name_width + type_char_width + 1
                    })
                    .max()
                    .unwrap_or(10);

                let effective_column_width = max_display_width + selection_indicator_width;
                let width_per_item = effective_column_width + inter_column_spacing;

                self.items_per_row = if width_per_item > 0 {
                    std::cmp::max(1, available_width / width_per_item)
                } else {
                    1
                };

                self.column_width = if self.items_per_row > 0 {
                    let total_spacing =
                        (self.items_per_row.saturating_sub(1)) * inter_column_spacing;
                    let width_for_content = available_width.saturating_sub(total_spacing);
                    width_for_content / self.items_per_row
                } else {
                    available_width
                };

                self.total_rows = self.candidates.len().div_ceil(self.items_per_row);

                debug!(
                    "Recalculated layout: column_width={}, items_per_row={}, total_rows={}",
                    self.column_width, self.items_per_row, self.total_rows
                );
            }
        }

        match mode {
            DisplayMode::Full => {
                self.render_all_items(&mut stdout)?;
            }
            DisplayMode::SelectionOnly => {
                self.render_selection_update(&mut stdout)?;
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
            "Displayed {} candidates in {} rows (total items: {}, has_more: {}) - mode: {:?}",
            self.candidates.len(),
            self.total_rows,
            self.total_items_count,
            self.has_more_items,
            mode
        );
        Ok(())
    }

    fn render_all_items(&self, stdout: &mut std::io::Stdout) -> Result<()> {
        for row in 0..self.total_rows {
            let mut items_displayed_in_row = 0;

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

                // Calculate the total width this column should occupy
                let column_total_width = self.column_width + 1; // column_width + selection indicator
                let column_end_position = (col + 1) * column_total_width + col * 2; // + inter-column spacing

                // Check if this column would exceed terminal width
                if column_end_position > self.terminal_width {
                    debug!(
                        "Skipping column {} to prevent overflow: end_position={}, terminal_width={}",
                        col, column_end_position, self.terminal_width
                    );
                    break;
                }

                self.render_item(stdout, candidate, is_selected, is_message_item)?;

                items_displayed_in_row += 1;

                // Add spacing between columns (except for the last column in the row)
                if col < self.items_per_row - 1 && index + 1 < self.candidates.len() {
                    queue!(stdout, Print("  "))?; // Two spaces between columns
                }
            }

            debug!(
                "Row {}: displayed {} items with fixed column alignment",
                row, items_displayed_in_row
            );

            if row < self.total_rows - 1 {
                queue!(stdout, cursor::MoveToNextLine(1))?;
            }
        }
        Ok(())
    }

    fn render_selection_update(&self, stdout: &mut std::io::Stdout) -> Result<()> {
        // Optimized approach: only redraw the items without clearing
        // Move to the start of the completion area and redraw in place
        if let Some(start_row) = self.display_start_row {
            execute!(stdout, cursor::MoveTo(0, start_row + 1))?;
            self.render_all_items(stdout)?;

            // Move cursor back to input position
            if let Some(start_col) = self.display_start_col {
                let prompt_width = unicode_display_width(&self.prompt_text);
                let input_width = unicode_display_width(&self.input_text);
                let input_end_col = start_col + prompt_width as u16 + input_width as u16;
                execute!(stdout, cursor::MoveTo(input_end_col, start_row))?;
            }
        } else {
            // Fallback to full display if position is unknown
            self.render_all_items(stdout)?;
        }

        Ok(())
    }

    fn render_item(
        &self,
        stdout: &mut std::io::Stdout,
        candidate: &Candidate,
        is_selected: bool,
        is_message_item: bool,
    ) -> Result<()> {
        // Display the selection indicator
        if is_selected {
            queue!(stdout, SetForegroundColor(Color::Yellow))?;
            queue!(stdout, Print(">"))?;
        } else {
            queue!(stdout, Print(" "))?;
        }

        // Format the item for display with fixed width
        let formatted = if is_message_item {
            // For message items, don't apply column width formatting
            candidate.get_display_name().to_string()
        } else {
            candidate.get_formatted_display(self.column_width)
        };

        // Add type-specific coloring
        if is_message_item {
            queue!(stdout, SetForegroundColor(Color::DarkGrey))?;
        } else {
            match candidate.get_type_char() {
                '⚡' => queue!(stdout, SetForegroundColor(Color::Green))?, // Command - lightning bolt
                '📁' => queue!(stdout, SetForegroundColor(Color::Blue))?,  // Directory - folder
                '📄' => queue!(stdout, SetForegroundColor(Color::White))?, // File - document
                '⚙' => queue!(stdout, SetForegroundColor(Color::Yellow))?, // Option - gear
                '🔹' => queue!(stdout, SetForegroundColor(Color::White))?, // Basic - small blue diamond
                '🌿' => queue!(stdout, SetForegroundColor(Color::Green))?, // Git branch - herb/branch
                '📜' => queue!(stdout, SetForegroundColor(Color::Cyan))?,  // Script - scroll
                '🕒' => queue!(stdout, SetForegroundColor(Color::Magenta))?, // History - clock
                _ => queue!(stdout, SetForegroundColor(Color::White))?,
            }
        }

        queue!(stdout, Print(formatted))?;
        queue!(stdout, ResetColor)?;

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

impl CompletionUi for CompletionDisplay {
    fn show(&mut self) -> Result<()> {
        self.display()
    }

    fn refresh_selection(&mut self) -> Result<()> {
        self.update_selection()
    }

    fn clear(&mut self) -> Result<()> {
        self.clear_display()
    }

    fn move_up(&mut self) {
        CompletionDisplay::move_up(self);
    }

    fn move_down(&mut self) {
        CompletionDisplay::move_down(self);
    }

    fn move_left(&mut self) {
        CompletionDisplay::move_left(self);
    }

    fn move_right(&mut self) {
        CompletionDisplay::move_right(self);
    }

    fn selected_output(&self) -> Option<String> {
        self.get_selected()
            .map(|candidate| candidate.output().to_string())
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

        // Ensure we have at least some space for the name
        if max_name_width < 3 {
            // If width is too small, just return the type character with padding
            let padding_needed = width.saturating_sub(type_char_width);
            return format!("{}{}", type_char, " ".repeat(padding_needed));
        }

        // Truncate name if it's too long for the available width
        let display_name = if unicode_display_width(name) > max_name_width {
            truncate_to_width(name, max_name_width)
        } else {
            name.to_string()
        };

        // Calculate padding needed to make the total width exactly match the requested width
        let name_display_width = unicode_display_width(&display_name);
        let content_width = type_char_width + 1 + name_display_width; // type_char + " " + name
        let padding_needed = width.saturating_sub(content_width);

        // Format with proper padding to ensure fixed width
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
            description: Some(format!("Description for {text}")),
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

    #[test]
    fn test_terminal_size_calculation() {
        // Test with various terminal widths to ensure proper calculation
        let candidates = vec![
            Candidate::Command {
                name: "git".to_string(),
                description: "Version control".to_string(),
            },
            Candidate::File {
                path: "file.txt".to_string(),
                is_dir: false,
            },
            Candidate::File {
                path: "directory/".to_string(),
                is_dir: true,
            },
        ];

        let config = CompletionConfig::default();
        let display = CompletionDisplay::new_with_config(candidates, "$ ", "test input", config);

        // Verify that items_per_row is reasonable
        assert!(display.items_per_row >= 1);
        assert!(display.items_per_row <= display.candidates.len());

        // Verify that column_width is reasonable
        assert!(display.column_width > 0);
        assert!(display.column_width <= display.terminal_width);

        // Verify that total_rows is calculated correctly
        let expected_rows = display.candidates.len().div_ceil(display.items_per_row);
        assert_eq!(display.total_rows, expected_rows);
    }

    #[test]
    fn test_formatted_display_width_limits() {
        let candidate = Candidate::Command {
            name: "very_long_command_name_that_should_be_truncated".to_string(),
            description: "A command with a very long name".to_string(),
        };

        // Test with small width
        let formatted = candidate.get_formatted_display(20);
        let display_width = unicode_display_width(&formatted);

        // Should not exceed the requested width significantly
        assert!(display_width <= 25); // Allow some tolerance for emoji width

        // Should contain the type character
        assert!(formatted.contains('⚡'));

        // Should be truncated if name is too long
        if candidate.get_display_name().len() > 15 {
            assert!(formatted.contains('…'));
        }
    }

    #[test]
    fn test_column_alignment_fixed_width() {
        // Test that formatted display produces consistent width
        let candidates = vec![
            Candidate::Command {
                name: "git".to_string(),
                description: "Version control".to_string(),
            },
            Candidate::Command {
                name: "very_long_command_name".to_string(),
                description: "A command with a long name".to_string(),
            },
            Candidate::File {
                path: "file.txt".to_string(),
                is_dir: false,
            },
        ];

        let fixed_width = 25;

        for candidate in &candidates {
            let formatted = candidate.get_formatted_display(fixed_width);
            let actual_width = unicode_display_width(&formatted);

            // All formatted items should have exactly the same width
            assert_eq!(
                actual_width,
                fixed_width,
                "Candidate '{}' has width {} but expected {}",
                candidate.get_display_name(),
                actual_width,
                fixed_width
            );
        }
    }

    #[test]
    fn test_column_alignment_with_unicode() {
        // Test column alignment with Unicode characters
        let candidates = vec![
            Candidate::File {
                path: "file.txt".to_string(),
                is_dir: false,
            },
            Candidate::File {
                path: "日本語ファイル.txt".to_string(), // Japanese filename
                is_dir: false,
            },
            Candidate::File {
                path: "🐕.txt".to_string(), // Emoji filename
                is_dir: false,
            },
        ];

        let fixed_width = 30;

        for candidate in &candidates {
            let formatted = candidate.get_formatted_display(fixed_width);
            let actual_width = unicode_display_width(&formatted);

            // All formatted items should have exactly the same width, even with Unicode
            assert_eq!(
                actual_width,
                fixed_width,
                "Unicode candidate '{}' has width {} but expected {}",
                candidate.get_display_name(),
                actual_width,
                fixed_width
            );
        }
    }
}
