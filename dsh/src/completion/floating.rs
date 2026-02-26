use super::display::{Candidate, CompletionConfig};
use super::ui::CompletionUi;
use anyhow::Result;
use crossterm::{
    cursor, execute,
    terminal::{Clear, ClearType},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use skim::SkimItem;
use std::io::{Write, stdout};

pub struct RatatuiCompletionUi {
    candidates: Vec<Candidate>,
    state: ListState,
    config: CompletionConfig,
    anchor: Option<(u16, u16)>,
    reserved_height: u16,
    selected_abs_index: Option<usize>,
    scroll_offset: usize,
}

impl RatatuiCompletionUi {
    pub fn new(candidates: Vec<Candidate>, config: CompletionConfig) -> Self {
        let selected_abs_index = if !candidates.is_empty() && config.max_items > 0 {
            Some(0)
        } else {
            None
        };

        let mut ui = Self {
            candidates,
            state: ListState::default(),
            config,
            anchor: None,
            reserved_height: 0,
            selected_abs_index,
            scroll_offset: 0,
        };
        ui.ensure_view_state();
        ui
    }

    fn total_len(&self) -> usize {
        self.candidates.len()
    }

    fn visible_capacity(&self) -> usize {
        self.config.max_items
    }

    fn visible_len(&self) -> usize {
        let cap = self.visible_capacity();
        if cap == 0 {
            return 0;
        }

        self.total_len().saturating_sub(self.scroll_offset).min(cap)
    }

    fn max_scroll_offset(&self) -> usize {
        let total = self.total_len();
        let cap = self.visible_capacity();
        if cap == 0 || total <= cap {
            0
        } else {
            total - cap
        }
    }

    fn completion_height(&self) -> u16 {
        let cap = self.visible_capacity();
        let items_len = if cap == 0 {
            0
        } else {
            self.total_len().min(cap)
        };
        let height = (items_len + 2) as u16; // +2 for borders
        height.max(2)
    }

    fn ensure_view_state(&mut self) {
        let total = self.total_len();
        let cap = self.visible_capacity();

        if total == 0 || cap == 0 {
            self.scroll_offset = 0;
            self.selected_abs_index = None;
            self.state.select(None);
            return;
        }

        let selected = self
            .selected_abs_index
            .unwrap_or(0)
            .min(total.saturating_sub(1));

        if selected < self.scroll_offset {
            self.scroll_offset = selected;
        } else if selected >= self.scroll_offset + cap {
            self.scroll_offset = selected + 1 - cap;
        }

        self.scroll_offset = self.scroll_offset.min(self.max_scroll_offset());

        let rel = selected.saturating_sub(self.scroll_offset);
        self.selected_abs_index = Some(selected);
        self.state.select(Some(rel));
    }

    fn set_selected_abs_index(&mut self, index: usize) {
        self.selected_abs_index = Some(index);
        self.ensure_view_state();
    }

    fn list_title(&self) -> String {
        let total = self.total_len();
        if total == 0 {
            return "Suggestions".to_string();
        }

        let selected = self.selected_abs_index.map_or(0, |i| i + 1);
        let cap = self.visible_capacity();
        if cap > 0 && total > cap {
            let start = self.scroll_offset + 1;
            let end = self.scroll_offset + self.visible_len();
            format!("Suggestions {selected}/{total} [{start}-{end}]")
        } else {
            format!("Suggestions {selected}/{total}")
        }
    }

    fn reserve_space_if_needed(&mut self) -> Result<()> {
        if self.anchor.is_some() {
            return Ok(());
        }

        let (col, row) = cursor::position().unwrap_or((0, 0));
        let height = self.completion_height();
        let mut out = stdout();
        for _ in 0..height {
            writeln!(out)?;
        }
        execute!(out, cursor::MoveUp(height), cursor::MoveTo(col, row))?;
        out.flush()?;
        self.anchor = Some((col, row));
        self.reserved_height = height;
        Ok(())
    }

    fn clear_reserved_area(&self) -> Result<()> {
        let Some((_, row)) = self.anchor else {
            return Ok(());
        };

        let mut out = stdout();
        let start_row = row.saturating_add(1);
        for offset in 0..self.reserved_height {
            execute!(
                out,
                cursor::MoveTo(0, start_row.saturating_add(offset)),
                Clear(ClearType::CurrentLine)
            )?;
        }
        out.flush()?;
        Ok(())
    }

    fn render_into_reserved_area(&mut self) -> Result<()> {
        let Some((anchor_col, anchor_row)) = self.anchor else {
            return Ok(());
        };

        self.ensure_view_state();
        self.clear_reserved_area()?;

        let out = stdout();
        let backend = CrosstermBackend::new(out);
        let mut terminal = Terminal::new(backend)?;

        terminal.draw(|f| {
            let frame = f.area();
            let render_y = anchor_row
                .saturating_add(1)
                .min(frame.y.saturating_add(frame.height.saturating_sub(1)));
            let available_height = frame
                .height
                .saturating_sub(render_y.saturating_sub(frame.y));
            let render_height = self.reserved_height.min(available_height).max(1);
            let area = Rect {
                x: frame.x,
                y: render_y,
                width: frame.width,
                height: render_height,
            };

            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
                .split(area);

            let items: Vec<ListItem> = self
                .candidates
                .iter()
                .skip(self.scroll_offset)
                .take(self.visible_capacity())
                .map(|c| {
                    let content = format!("{} {}", c.get_type_char(), c.get_display_name());
                    ListItem::new(Line::from(Span::raw(content)))
                })
                .collect();

            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(self.list_title()),
                )
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol(">> ");

            f.render_stateful_widget(list, chunks[0], &mut self.state);

            // Right pane: Description of selected item
            let description = self
                .selected_abs_index
                .and_then(|i| self.candidates.get(i))
                .and_then(|c| c.get_description())
                .unwrap_or("");

            let desc_widget = Paragraph::new(description)
                .block(Block::default().borders(Borders::ALL).title("Description"))
                .wrap(Wrap { trim: true });
            f.render_widget(desc_widget, chunks[1]);
        })?;

        let mut out = stdout();
        execute!(out, cursor::MoveTo(anchor_col, anchor_row))?;
        out.flush()?;

        Ok(())
    }
}

impl CompletionUi for RatatuiCompletionUi {
    fn show(&mut self) -> Result<()> {
        self.ensure_view_state();
        self.reserve_space_if_needed()?;
        self.render_into_reserved_area()
    }

    fn refresh_selection(&mut self) -> Result<()> {
        self.ensure_view_state();
        if self.anchor.is_none() {
            self.reserve_space_if_needed()?;
        }
        self.render_into_reserved_area()
    }

    fn clear(&mut self) -> Result<()> {
        if self.anchor.is_none() {
            return Ok(());
        }

        self.clear_reserved_area()?;
        if let Some((col, row)) = self.anchor {
            let mut out = stdout();
            execute!(out, cursor::MoveTo(col, row))?;
            out.flush()?;
        }
        self.anchor = None;
        self.reserved_height = 0;
        Ok(())
    }

    fn move_up(&mut self) {
        let total = self.total_len();
        if total == 0 || self.visible_capacity() == 0 {
            self.selected_abs_index = None;
            self.ensure_view_state();
            return;
        }

        let current = self.selected_abs_index.unwrap_or(0).min(total - 1);
        let next = if current == 0 { total - 1 } else { current - 1 };
        self.set_selected_abs_index(next);
    }

    fn move_down(&mut self) {
        let total = self.total_len();
        if total == 0 || self.visible_capacity() == 0 {
            self.selected_abs_index = None;
            self.ensure_view_state();
            return;
        }

        let current = self.selected_abs_index.unwrap_or(0).min(total - 1);
        let next = if current + 1 >= total { 0 } else { current + 1 };
        self.set_selected_abs_index(next);
    }

    fn move_left(&mut self) {
        // No-op for a vertical list
    }

    fn move_right(&mut self) {
        // No-op for a vertical list
    }

    fn move_page_up(&mut self) {
        let total = self.total_len();
        let step = self.visible_capacity();
        if total == 0 || step == 0 {
            self.selected_abs_index = None;
            self.ensure_view_state();
            return;
        }

        let current = self.selected_abs_index.unwrap_or(0).min(total - 1);
        let shift = step % total;
        let next = (current + total - shift) % total;
        self.set_selected_abs_index(next);
    }

    fn move_page_down(&mut self) {
        let total = self.total_len();
        let step = self.visible_capacity();
        if total == 0 || step == 0 {
            self.selected_abs_index = None;
            self.ensure_view_state();
            return;
        }

        let current = self.selected_abs_index.unwrap_or(0).min(total - 1);
        let next = (current + (step % total)) % total;
        self.set_selected_abs_index(next);
    }

    fn move_home(&mut self) {
        if self.total_len() == 0 || self.visible_capacity() == 0 {
            self.selected_abs_index = None;
            self.ensure_view_state();
            return;
        }

        self.set_selected_abs_index(0);
    }

    fn move_end(&mut self) {
        let total = self.total_len();
        if total == 0 || self.visible_capacity() == 0 {
            self.selected_abs_index = None;
            self.ensure_view_state();
            return;
        }

        self.set_selected_abs_index(total - 1);
    }

    fn selected_output(&self) -> Option<String> {
        self.selected_abs_index
            .and_then(|i| self.candidates.get(i))
            .map(|c| c.output().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::display::{Candidate, CompletionConfig};

    fn setup_ui(count: usize) -> RatatuiCompletionUi {
        let candidates = (0..count)
            .map(|i| Candidate::Basic(format!("item_{}", i)))
            .collect();
        RatatuiCompletionUi::new(candidates, CompletionConfig::default())
    }

    fn setup_ui_with_max_items(count: usize, max_items: usize) -> RatatuiCompletionUi {
        let candidates = (0..count)
            .map(|i| Candidate::Basic(format!("item_{}", i)))
            .collect();
        RatatuiCompletionUi::new(
            candidates,
            CompletionConfig {
                max_items,
                ..CompletionConfig::default()
            },
        )
    }

    #[test]
    fn test_initial_selection() {
        let ui = setup_ui(3);
        assert_eq!(ui.selected_abs_index, Some(0));
        assert_eq!(ui.state.selected(), Some(0));
        assert_eq!(ui.selected_output(), Some("item_0".to_string()));

        let empty_ui = setup_ui(0);
        assert_eq!(empty_ui.selected_abs_index, None);
        assert_eq!(empty_ui.state.selected(), None);
    }

    #[test]
    fn test_move_down_and_wrap() {
        let mut ui = setup_ui(3);

        ui.move_down();
        assert_eq!(ui.selected_abs_index, Some(1));

        ui.move_down();
        assert_eq!(ui.selected_abs_index, Some(2));

        // Wrap around to 0
        ui.move_down();
        assert_eq!(ui.selected_abs_index, Some(0));
    }

    #[test]
    fn test_move_up_and_wrap() {
        let mut ui = setup_ui(3); // initially at 0

        // Wrap around to last
        ui.move_up();
        assert_eq!(ui.selected_abs_index, Some(2));

        ui.move_up();
        assert_eq!(ui.selected_abs_index, Some(1));

        ui.move_up();
        assert_eq!(ui.selected_abs_index, Some(0));
    }

    #[test]
    fn test_move_scrolls_through_all_candidates_with_small_window() {
        let mut ui = setup_ui_with_max_items(5, 2);

        assert_eq!(ui.selected_abs_index, Some(0));
        assert_eq!(ui.scroll_offset, 0);
        assert_eq!(ui.state.selected(), Some(0));

        ui.move_down();
        assert_eq!(ui.selected_abs_index, Some(1));
        assert_eq!(ui.scroll_offset, 0);
        assert_eq!(ui.state.selected(), Some(1));

        ui.move_down();
        assert_eq!(ui.selected_abs_index, Some(2));
        assert_eq!(ui.scroll_offset, 1);
        assert_eq!(ui.state.selected(), Some(1));

        ui.move_down();
        assert_eq!(ui.selected_abs_index, Some(3));
        assert_eq!(ui.scroll_offset, 2);
        assert_eq!(ui.state.selected(), Some(1));

        ui.move_down();
        assert_eq!(ui.selected_abs_index, Some(4));
        assert_eq!(ui.scroll_offset, 3);
        assert_eq!(ui.state.selected(), Some(1));

        ui.move_down();
        assert_eq!(ui.selected_abs_index, Some(0));
        assert_eq!(ui.scroll_offset, 0);
        assert_eq!(ui.state.selected(), Some(0));
    }

    #[test]
    fn test_page_navigation_and_home_end() {
        let mut ui = setup_ui_with_max_items(5, 2);

        ui.move_page_down();
        assert_eq!(ui.selected_abs_index, Some(2));

        ui.move_page_down();
        assert_eq!(ui.selected_abs_index, Some(4));

        ui.move_page_down();
        assert_eq!(ui.selected_abs_index, Some(1));

        ui.move_page_up();
        assert_eq!(ui.selected_abs_index, Some(4));

        ui.move_home();
        assert_eq!(ui.selected_abs_index, Some(0));

        ui.move_end();
        assert_eq!(ui.selected_abs_index, Some(4));
    }

    #[test]
    fn test_move_is_noop_when_no_visible_items() {
        let mut ui = setup_ui_with_max_items(2, 0);
        assert_eq!(ui.selected_abs_index, None);
        assert_eq!(ui.state.selected(), None);

        ui.move_down();
        assert_eq!(ui.selected_abs_index, None);
        assert_eq!(ui.state.selected(), None);

        ui.move_up();
        assert_eq!(ui.selected_abs_index, None);
        assert_eq!(ui.state.selected(), None);

        ui.move_page_down();
        assert_eq!(ui.selected_abs_index, None);

        ui.move_page_up();
        assert_eq!(ui.selected_abs_index, None);

        assert_eq!(ui.selected_output(), None);
    }

    #[test]
    fn test_selected_output_uses_absolute_selection() {
        let mut ui = setup_ui_with_max_items(4, 2);
        ui.selected_abs_index = Some(3);
        ui.ensure_view_state();

        assert_eq!(ui.selected_output(), Some("item_3".to_string()));
        assert_eq!(ui.scroll_offset, 2);
        assert_eq!(ui.state.selected(), Some(1));
    }
}
