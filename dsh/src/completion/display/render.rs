//! Drawing the completion grid: the full redraw and the cheaper selection-only repaint (`display`/`update_selection`), the per-item and detail-line rendering they share, and clearing the grid off the
//! terminal (`clear_display`).
use super::*;

impl CompletionDisplay {
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

        // Detail line for the highlighted candidate, just below the grid.
        self.render_detail_line(&mut renderer, &layout)?;

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
            // The cursor is returned to the input position by the caller
            // (`display_with_mode`) after the detail line has been drawn.
        } else {
            // Fallback to full display if position is unknown
            self.render_all_items(writer, layout)?;
        }

        Ok(())
    }
    /// Render the detail line for the highlighted candidate directly below the
    /// grid. Reuses the same dimmed style as the rest of the auxiliary text.
    /// A no-op when no detail line is reserved for this session.
    fn render_detail_line(
        &self,
        writer: &mut TerminalRenderer,
        layout: &LayoutCache,
    ) -> Result<()> {
        if !self.show_detail_line {
            return Ok(());
        }

        let text = self
            .get_selected()
            .and_then(detail_line_text)
            .unwrap_or_default();
        let truncated = truncate_to_width(&text, layout.terminal_width.saturating_sub(1));

        queue!(
            writer,
            cursor::MoveToNextLine(1),
            cursor::MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(Color::DarkGrey),
            Print(truncated),
            ResetColor
        )?;
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
        let (formatted, description_part) = if is_message_item {
            // For message items, don't apply column width formatting
            (candidate.get_display_name().to_string(), None)
        } else {
            candidate.get_formatted_display(layout.column_width, layout.max_name_width)
        };

        // Add type-specific coloring
        if is_message_item {
            queue!(writer, SetForegroundColor(Color::DarkGrey))?;
        } else {
            match candidate.get_type_char() {
                '⚡' => queue!(writer, SetForegroundColor(Color::Yellow))?, // Command - lightning bolt
                '📁' => queue!(writer, SetForegroundColor(Color::Blue))?,   // Directory - folder
                '📄' => queue!(writer, SetForegroundColor(Color::White))?,  // File - document
                '⚙' => queue!(writer, SetForegroundColor(Color::Cyan))?,    // Option - gear
                '🔹' => queue!(writer, SetForegroundColor(Color::White))?, // Basic - small blue diamond
                '🌿' => queue!(writer, SetForegroundColor(Color::Green))?, // Git branch - herb/branch
                '📜' => queue!(writer, SetForegroundColor(Color::Yellow))?, // Script - scroll
                '🕒' => queue!(writer, SetForegroundColor(Color::Magenta))?, // History - clock
                _ => queue!(writer, SetForegroundColor(Color::White))?,
            }
        }

        queue!(writer, Print(formatted))?;

        // Render description if available (dimmed)
        if let Some(desc) = description_part {
            queue!(writer, SetForegroundColor(Color::DarkGrey))?;
            queue!(writer, Print(desc))?;
        }

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

        // Include the detail line (if any) below the grid in the rows to clear.
        let rows_to_clear = layout.total_rows + self.detail_rows() as usize;

        if let (Some(start_row), Some(start_col)) = (self.display_start_row, self.display_start_col)
        {
            debug!(
                "Using recorded position: col={}, row={}",
                start_col, start_row
            );

            queue!(renderer, cursor::MoveTo(start_col, start_row))?;
            queue!(renderer, Clear(ClearType::CurrentLine))?;

            for i in 0..rows_to_clear {
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
            for i in 0..rows_to_clear {
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
