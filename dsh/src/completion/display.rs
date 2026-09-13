use super::ui::CompletionUi;
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{cursor, execute, queue};
use serde::{Deserialize, Serialize};
use skim::SkimItem;
use std::io::{Write, stdout};
use tracing::{debug, warn};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod layout;
mod render;

#[derive(Debug, Clone, Copy)]
enum DisplayMode {
    Full,
    SelectionOnly,
}

// Completion display configuration
const MAX_COMPLETION_ITEMS: usize = 30;
const COMPLETION_MAX_ITEMS_ENV: &str = "DSH_COMPLETION_MAX_ITEMS";

fn default_max_completion_items() -> usize {
    std::env::var(COMPLETION_MAX_ITEMS_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(MAX_COMPLETION_ITEMS)
}

fn unicode_display_width(s: &str) -> usize {
    s.width()
}

/// Truncate a Unicode string to fit within the specified display width
fn truncate_to_width(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

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
            max_items: default_max_completion_items(),
            more_items_message_template: "...and {} more items available".to_string(),
            show_item_count: true,
        }
    }
}

impl CompletionConfig {
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
    has_more_items: bool,
    total_items_count: usize,
    /// Whether to reserve a detail line below the grid for the highlighted
    /// candidate's description. Decided once per session (constant while the
    /// grid is open) so the rendered geometry does not change as the selection
    /// moves. Enabled when at least one candidate carries a description.
    show_detail_line: bool,
}

/// Build the one-line detail text shown for the highlighted candidate:
/// `<name> — <description>`. Returns `None` when the candidate has no
/// description worth showing.
fn detail_line_text(candidate: &Candidate) -> Option<String> {
    let description = candidate.get_description()?;
    let name = candidate.get_display_name();
    Some(format!("{name} — {description}"))
}

#[derive(Debug, Clone)]
pub(crate) struct LayoutCache {
    terminal_width: usize,
    column_width: usize,
    max_name_width: usize, // Added to align descriptions
    items_per_row: usize,
    total_rows: usize,
}

const FALLBACK_LAYOUT: LayoutCache = LayoutCache {
    terminal_width: 80,
    column_width: 80,
    max_name_width: 80,
    items_per_row: 1,
    total_rows: 0,
};

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

        // Reserve a detail line only when some candidate actually has a
        // description to show (e.g. commands/options), so plain file/path
        // completion is not pushed up by an always-empty line.
        let show_detail_line = candidates.iter().any(|c| c.get_description().is_some());

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
            has_more_items,
            total_items_count,
            show_detail_line,
        }
    }

    fn detail_rows(&self) -> u16 {
        if self.show_detail_line { 1 } else { 0 }
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
        let needed_rows = layout.total_rows as u16 + self.detail_rows();

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
    Process {
        pid: String,
        command: String,
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
            Candidate::Process { .. } => '🔧',   // Process - tool/gear
        }
    }

    /// Get the description of the candidate
    pub fn get_description(&self) -> Option<&str> {
        match self {
            Candidate::Item(_, desc) if !desc.is_empty() => Some(desc),
            Candidate::Command { description, .. } if !description.is_empty() => Some(description),
            Candidate::Option { description, .. } if !description.is_empty() => Some(description),
            Candidate::History { .. } => None, // Could format frequency here if desired
            Candidate::Process { command, .. } => Some(command),
            _ => None,
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
            Candidate::Process { pid, .. } => pid,
        }
    }

    /// Get formatted display string with type character and description.
    ///
    /// The returned name part plus optional description part always occupies
    /// exactly `width` display cells. `render_item` prints the selection
    /// indicator separately, so the full rendered cell is `width + 1` cells.
    pub fn get_formatted_display(
        &self,
        width: usize,
        max_name_width: usize,
    ) -> (String, Option<String>) {
        let type_char = self.get_type_char();
        let name = self.get_display_name();

        let type_char_width = type_char.width().unwrap_or(2);

        let mut result_name = String::new();

        let max_content_width = width.saturating_sub(type_char_width + 1);

        // Truncate name if it exceeds available width
        let display_name = if unicode_display_width(name) > max_content_width {
            truncate_to_width(name, max_content_width)
        } else {
            name.to_string()
        };

        result_name.push(type_char);
        result_name.push(' ');
        result_name.push_str(&display_name);

        // Pad name to align description (or to fill column width)
        let current_width = type_char_width + 1 + unicode_display_width(&display_name);
        let padding_needed = if max_name_width > 0 {
            // Align to max_name_width if meaningful
            // But don't exceed total width
            let target = max_name_width.min(width);
            target.saturating_sub(current_width)
        } else {
            width.saturating_sub(current_width)
        };

        result_name.push_str(&" ".repeat(padding_needed));

        let description_part = if let Some(desc) = self.get_description() {
            let used_width = current_width + padding_needed;
            let remaining_width = width.saturating_sub(used_width);

            if remaining_width > 3 {
                let desc_width = remaining_width.saturating_sub(2);
                let actual_desc = truncate_to_width(desc, desc_width);
                let actual_desc_width = unicode_display_width(&actual_desc);
                let trailing_padding = desc_width.saturating_sub(actual_desc_width);
                Some(format!("  {}{}", actual_desc, " ".repeat(trailing_padding)))
            } else {
                if remaining_width > 0 {
                    result_name.push_str(&" ".repeat(remaining_width));
                }
                None
            }
        } else {
            // Fill remaining space with whitespace to ensure background color consistency if selected
            let used_width = current_width + padding_needed;
            let remaining = width.saturating_sub(used_width);
            if remaining > 0 {
                result_name.push_str(&" ".repeat(remaining));
            }
            None
        };

        #[cfg(debug_assertions)]
        {
            let rendered_width = unicode_display_width(&result_name)
                + description_part
                    .as_deref()
                    .map(unicode_display_width)
                    .unwrap_or(0);
            debug_assert_eq!(rendered_width, width);
        }

        (result_name, description_part)
    }
}

#[cfg(test)]
mod tests;
