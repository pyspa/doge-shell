use super::ui::CompletionUi;
use crate::terminal::renderer::TerminalRenderer;
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
    layout_cache: Option<LayoutCache>,
    layout_dirty: bool,
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

#[derive(Debug, Clone)]
pub(crate) struct LayoutCache {
    terminal_width: usize,
    column_width: usize,
    items_per_row: usize,
    total_rows: usize,
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
    #[cfg(test)]
    pub(crate) fn force_layout(&mut self, terminal_width: usize) -> LayoutCache {
        let cache = self.calculate_layout(terminal_width);
        self.layout_cache = Some(cache.clone());
        self.layout_dirty = false;
        cache
    }

    fn calculate_layout(&self, terminal_width: usize) -> LayoutCache {
        // Calculate the maximum display width needed for each candidate
        let max_display_width = self
            .candidates
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

            std::cmp::min(max_items, self.candidates.len().max(1))
        } else {
            1
        };

        // Recalculate column width based on actual items per row to ensure proper fit
        let column_width = if items_per_row > 0 {
            let available_width = terminal_width.saturating_sub(4); // Reserve margin
            let total_spacing = (items_per_row.saturating_sub(1)) * inter_column_spacing;
            let width_for_content = available_width.saturating_sub(total_spacing);
            width_for_content.max(1) / items_per_row.max(1)
        } else {
            terminal_width.saturating_sub(4)
        };

        let total_rows = self.candidates.len().div_ceil(items_per_row.max(1));

        debug!(
            "Display layout: terminal_width={}, column_width={}, items_per_row={}, total_rows={}",
            terminal_width, column_width, items_per_row, total_rows
        );

        LayoutCache {
            terminal_width,
            column_width,
            items_per_row: items_per_row.max(1),
            total_rows,
        }
    }

    fn ensure_layout(&mut self, terminal_width: usize) {
        let needs_recalc = self.layout_dirty
            || self
                .layout_cache
                .as_ref()
                .is_none_or(|cache| cache.terminal_width != terminal_width);
        if needs_recalc {
            let cache = self.calculate_layout(terminal_width);
            self.layout_cache = Some(cache);
            self.layout_dirty = false;
        }
    }

    fn layout(&self) -> &LayoutCache {
        self.layout_cache
            .as_ref()
            .expect("layout must be prepared before rendering")
    }

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

        CompletionDisplay {
            candidates,
            selected_index: 0,
            layout_cache: None,
            layout_dirty: true,
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
    fn ensure_display_space(
        &mut self,
        layout: &LayoutCache,
        renderer: &mut TerminalRenderer,
    ) -> Result<()> {
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
        let needed_rows = layout.total_rows as u16;

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
            queue!(
                renderer,
                cursor::MoveTo(0, terminal_height.saturating_sub(1))
            )?;
            for _ in 0..rows_to_create {
                queue!(renderer, Print("\n"))?;
            }

            // Update our recorded position since content has shifted up
            let new_row = original_row.saturating_sub(rows_to_create);

            self.display_start_row = Some(new_row);
            debug!("Updated display start position to row: {}", new_row);

            // Move cursor back to the updated position
            queue!(renderer, cursor::MoveTo(original_col, new_row))?;
        }

        Ok(())
    }

    pub fn move_up(&mut self) {
        if let Some(layout) = self.layout_cache.as_ref()
            && self.selected_index >= layout.items_per_row
        {
            self.selected_index -= layout.items_per_row;
        }
    }

    pub fn move_down(&mut self) {
        if let Some(layout) = self.layout_cache.as_ref()
            && self.selected_index + layout.items_per_row < self.candidates.len()
        {
            self.selected_index += layout.items_per_row;
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
        let (current_terminal_width, _) = crossterm::terminal::size()?;
        let current_terminal_width = current_terminal_width as usize;
        self.ensure_layout(current_terminal_width);
        let layout = self.layout().clone();

        let mut renderer = TerminalRenderer::new();

        // Hide cursor during completion display (only once)
        if !self.cursor_hidden {
            queue!(renderer, cursor::Hide)?;
            self.cursor_hidden = true;
        }

        match mode {
            DisplayMode::Full => {
                if self.display_start_row.is_none() {
                    if let Ok((col, row)) = cursor::position() {
                        debug!("Recording display start position: col={}, row={}", col, row);
                        self.display_start_row = Some(row);
                        self.display_start_col = Some(col);
                    } else {
                        debug!("Failed to get cursor position");
                    }
                }

                self.ensure_display_space(&layout, &mut renderer)?;

                queue!(
                    renderer,
                    cursor::MoveToColumn(0),
                    Clear(ClearType::CurrentLine)
                )?;
                queue!(renderer, Print(&self.prompt_text))?;
                queue!(renderer, Print(&self.input_text))?;
                queue!(renderer, cursor::MoveToNextLine(1))?;
            }
            DisplayMode::SelectionOnly => {
                if let Some(start_row) = self.display_start_row {
                    queue!(renderer, cursor::MoveTo(0, start_row + 1))?;
                } else {
                    return self.display_with_mode(DisplayMode::Full);
                }
            }
        }

        match mode {
            DisplayMode::Full => {
                self.render_all_items(&mut renderer, &layout)?;
            }
            DisplayMode::SelectionOnly => {
                self.render_selection_update(&mut renderer, &layout)?;
            }
        }

        if let (Some(start_row), Some(start_col)) = (self.display_start_row, self.display_start_col)
        {
            let prompt_width = unicode_display_width(&self.prompt_text);
            let input_width = unicode_display_width(&self.input_text);
            let input_end_col = start_col + prompt_width as u16 + input_width as u16;
            queue!(renderer, cursor::MoveTo(input_end_col, start_row))?;
        }

        renderer.flush()?;
        debug!(
            "Displayed {} candidates in {} rows (total items: {}, has_more: {}) - mode: {:?}",
            self.candidates.len(),
            layout.total_rows,
            self.total_items_count,
            self.has_more_items,
            mode
        );
        Ok(())
    }

    fn render_all_items(&self, writer: &mut impl Write, layout: &LayoutCache) -> Result<()> {
        for row in 0..layout.total_rows {
            let mut items_displayed_in_row = 0;

            for col in 0..layout.items_per_row {
                let index = row * layout.items_per_row + col;
                if index >= self.candidates.len() {
                    break;
                }

                let candidate = &self.candidates[index];
                let is_selected = index == self.selected_index;
                let is_message_item = self.has_more_items
                    && index == self.candidates.len() - 1
                    && candidate.get_display_name().starts_with("📋");

                // Calculate the total width this column should occupy
                let column_total_width = layout.column_width + 1; // column_width + selection indicator
                let column_end_position = (col + 1) * column_total_width + col * 2; // + inter-column spacing

                // Check if this column would exceed terminal width
                if column_end_position > layout.terminal_width {
                    debug!(
                        "Skipping column {} to prevent overflow: end_position={}, terminal_width={}",
                        col, column_end_position, layout.terminal_width
                    );
                    break;
                }

                self.render_item(writer, layout, candidate, is_selected, is_message_item)?;

                items_displayed_in_row += 1;

                // Add spacing between columns (except for the last column in the row)
                if col < layout.items_per_row - 1 && index + 1 < self.candidates.len() {
                    queue!(writer, Print("  "))?; // Two spaces between columns
                }
            }

            debug!(
                "Row {}: displayed {} items with fixed column alignment",
                row, items_displayed_in_row
            );

            if row < layout.total_rows - 1 {
                queue!(writer, cursor::MoveToNextLine(1))?;
            }
        }
        Ok(())
    }

    fn render_selection_update(
        &self,
        writer: &mut TerminalRenderer,
        layout: &LayoutCache,
    ) -> Result<()> {
        // Optimized approach: only redraw the items without clearing
        // Move to the start of the completion area and redraw in place
        if let Some(start_row) = self.display_start_row {
            queue!(writer, cursor::MoveTo(0, start_row + 1))?;
            self.render_all_items(writer, layout)?;

            // Move cursor back to input position
            if let Some(start_col) = self.display_start_col {
                let prompt_width = unicode_display_width(&self.prompt_text);
                let input_width = unicode_display_width(&self.input_text);
                let input_end_col = start_col + prompt_width as u16 + input_width as u16;
                queue!(writer, cursor::MoveTo(input_end_col, start_row))?;
            }
        } else {
            // Fallback to full display if position is unknown
            self.render_all_items(writer, layout)?;
        }

        Ok(())
    }

    fn render_item(
        &self,
        writer: &mut impl Write,
        layout: &LayoutCache,
        candidate: &Candidate,
        is_selected: bool,
        is_message_item: bool,
    ) -> Result<()> {
        // Display the selection indicator
        if is_selected {
            queue!(writer, SetForegroundColor(Color::Yellow))?;
            queue!(writer, Print(">"))?;
        } else {
            queue!(writer, Print(" "))?;
        }

        // Format the item for display with fixed width
        let formatted = if is_message_item {
            // For message items, don't apply column width formatting
            candidate.get_display_name().to_string()
        } else {
            candidate.get_formatted_display(layout.column_width)
        };

        // Add type-specific coloring
        if is_message_item {
            queue!(writer, SetForegroundColor(Color::DarkGrey))?;
        } else {
            match candidate.get_type_char() {
                '⚡' => queue!(writer, SetForegroundColor(Color::Green))?, // Command - lightning bolt
                '📁' => queue!(writer, SetForegroundColor(Color::Blue))?,  // Directory - folder
                '📄' => queue!(writer, SetForegroundColor(Color::White))?, // File - document
                '⚙' => queue!(writer, SetForegroundColor(Color::Yellow))?, // Option - gear
                '🔹' => queue!(writer, SetForegroundColor(Color::White))?, // Basic - small blue diamond
                '🌿' => queue!(writer, SetForegroundColor(Color::Green))?, // Git branch - herb/branch
                '📜' => queue!(writer, SetForegroundColor(Color::Cyan))?,  // Script - scroll
                '🕒' => queue!(writer, SetForegroundColor(Color::Magenta))?, // History - clock
                _ => queue!(writer, SetForegroundColor(Color::White))?,
            }
        }

        queue!(writer, Print(formatted))?;
        queue!(writer, ResetColor)?;

        Ok(())
    }

    pub fn clear_display(&mut self) -> Result<()> {
        let Some(layout) = self.layout_cache.clone() else {
            return Ok(());
        };

        debug!(
            "Clearing completion display with {} rows",
            layout.total_rows
        );

        let mut renderer = TerminalRenderer::new();

        if let (Some(start_row), Some(start_col)) = (self.display_start_row, self.display_start_col)
        {
            debug!(
                "Using recorded position: col={}, row={}",
                start_col, start_row
            );

            queue!(renderer, cursor::MoveTo(start_col, start_row))?;
            queue!(renderer, Clear(ClearType::CurrentLine))?;

            for i in 0..layout.total_rows {
                queue!(
                    renderer,
                    cursor::MoveToNextLine(1),
                    Clear(ClearType::CurrentLine)
                )?;
                debug!("Cleared completion line {}", i + 1);
            }

            queue!(renderer, cursor::MoveTo(start_col, start_row))?;
            queue!(renderer, Print(&self.prompt_text))?;
            queue!(renderer, Print(&self.input_text))?;

            let prompt_width = unicode_display_width(&self.prompt_text);
            let input_width = unicode_display_width(&self.input_text);
            let input_end_col = start_col + prompt_width as u16 + input_width as u16;
            queue!(renderer, cursor::MoveTo(input_end_col, start_row))?;
        } else {
            debug!("Using fallback clear method");

            queue!(renderer, Clear(ClearType::CurrentLine))?;
            for i in 0..layout.total_rows {
                queue!(
                    renderer,
                    cursor::MoveToPreviousLine(1),
                    Clear(ClearType::CurrentLine)
                )?;
                debug!("Cleared line {} (moving up)", i + 1);
            }

            queue!(renderer, Print(&self.prompt_text))?;
            queue!(renderer, Print(&self.input_text))?;
        }

        if self.cursor_hidden {
            queue!(renderer, cursor::Show)?;
            self.cursor_hidden = false;
        }

        self.display_start_row = None;
        self.display_start_col = None;

        renderer.flush()?;
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
                    ' ' // Option or other - gear
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
            Candidate::Option { .. } => ' ', // Option - gear
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
        let mut display =
            CompletionDisplay::new_with_config(candidates, "$ ", "test input", config);
        let layout = display.force_layout(80);

        // Verify that items_per_row is reasonable
        assert!(layout.items_per_row >= 1);
        assert!(layout.items_per_row <= display.candidates.len().max(1));

        // Verify that column_width is reasonable
        assert!(layout.column_width > 0);
        assert!(layout.column_width <= layout.terminal_width);

        // Verify that total_rows is calculated correctly
        let expected_rows = display
            .candidates
            .len()
            .div_ceil(layout.items_per_row.max(1));
        assert_eq!(layout.total_rows, expected_rows);
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
