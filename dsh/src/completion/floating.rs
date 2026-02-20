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
    widgets::{Block, Borders, List, ListItem, ListState},
};
use skim::SkimItem;
use std::io::{Write, stdout};

pub struct RatatuiCompletionUi {
    candidates: Vec<Candidate>,
    state: ListState,
    config: CompletionConfig,
    list_area: Option<Rect>,
}

impl RatatuiCompletionUi {
    pub fn new(candidates: Vec<Candidate>, config: CompletionConfig) -> Self {
        let mut state = ListState::default();
        if !candidates.is_empty() {
            state.select(Some(0));
        }
        Self {
            candidates,
            state,
            config,
            list_area: None,
        }
    }

    fn render_ui(&mut self) -> Result<()> {
        let stdout = stdout();
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // Move cursor down to print the UI inline without EnterAlternateScreen
        let items_len = self.candidates.len().min(self.config.max_items);
        let height = (items_len + 2) as u16; // +2 for borders

        // Very basic inline rendering layout (Phase 1)
        // We print empty lines to reserve space, then move cursor up
        let out = terminal.backend_mut();
        for _ in 0..height {
            writeln!(out)?;
        }
        execute!(out, cursor::MoveUp(height))?;

        terminal.draw(|f| {
            // Define an area below the current cursor
            let mut area = f.area();
            area.height = height;
            area.y = crossterm::cursor::position().unwrap_or((0, area.y)).1;

            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
                .split(area);

            self.list_area = Some(chunks[0]);

            let items: Vec<ListItem> = self
                .candidates
                .iter()
                .take(self.config.max_items)
                .map(|c| {
                    let content = format!("{} {}", c.get_type_char(), c.get_display_name());
                    ListItem::new(Line::from(Span::raw(content)))
                })
                .collect();

            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title("Suggestions"))
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol(">> ");

            f.render_stateful_widget(list, chunks[0], &mut self.state);

            // Right pane: Description of selected item
            let description = self
                .state
                .selected()
                .and_then(|i| self.candidates.get(i))
                .and_then(|c| c.get_description())
                .unwrap_or("");

            let desc_widget = ratatui::widgets::Paragraph::new(description)
                .block(Block::default().borders(Borders::ALL).title("Description"))
                .wrap(ratatui::widgets::Wrap { trim: true });
            f.render_widget(desc_widget, chunks[1]);
        })?;

        Ok(())
    }
}

impl CompletionUi for RatatuiCompletionUi {
    fn show(&mut self) -> Result<()> {
        self.render_ui()
    }

    fn refresh_selection(&mut self) -> Result<()> {
        self.render_ui()
    }

    fn clear(&mut self) -> Result<()> {
        let items_len = self.candidates.len().min(self.config.max_items);
        let height = (items_len + 2) as u16;
        let mut out = stdout();
        // Clear the reserved space
        for _ in 0..height {
            let _ = execute!(out, Clear(ClearType::CurrentLine));
            let _ = execute!(out, cursor::MoveDown(1));
        }
        let _ = execute!(out, cursor::MoveUp(height));
        Ok(())
    }

    fn move_up(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.candidates.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn move_down(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.candidates.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn move_left(&mut self) {
        // No-op for a vertical list
    }

    fn move_right(&mut self) {
        // No-op for a vertical list
    }

    fn handle_click(&mut self, col: u16, row: u16) -> Option<usize> {
        if let Some(area) = self.list_area {
            // Check if click is inside the list area, excluding borders
            if col > area.x
                && col < area.x + area.width.saturating_sub(1)
                && row > area.y
                && row < area.y + area.height.saturating_sub(1)
            {
                // Calculate item index based on click row
                let clicked_index = (row - area.y - 1) as usize;

                if clicked_index < self.candidates.len().min(self.config.max_items) {
                    return Some(clicked_index);
                }
            }
        }
        None
    }

    fn set_selection(&mut self, index: usize) {
        if index < self.candidates.len() {
            self.state.select(Some(index));
        }
    }

    fn selected_output(&self) -> Option<String> {
        self.state
            .selected()
            .and_then(|i| self.candidates.get(i))
            .map(|c| c.output().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::display::{Candidate, CompletionConfig};
    use ratatui::layout::Rect;

    fn setup_ui(count: usize) -> RatatuiCompletionUi {
        let candidates = (0..count)
            .map(|i| Candidate::Basic(format!("item_{}", i)))
            .collect();
        RatatuiCompletionUi::new(candidates, CompletionConfig::default())
    }

    #[test]
    fn test_initial_selection() {
        let ui = setup_ui(3);
        assert_eq!(ui.state.selected(), Some(0));
        assert_eq!(ui.selected_output(), Some("item_0".to_string()));

        let empty_ui = setup_ui(0);
        assert_eq!(empty_ui.state.selected(), None);
    }

    #[test]
    fn test_move_down_and_wrap() {
        let mut ui = setup_ui(3);

        ui.move_down();
        assert_eq!(ui.state.selected(), Some(1));

        ui.move_down();
        assert_eq!(ui.state.selected(), Some(2));

        // Wrap around to 0
        ui.move_down();
        assert_eq!(ui.state.selected(), Some(0));
    }

    #[test]
    fn test_move_up_and_wrap() {
        let mut ui = setup_ui(3); // initially at 0

        // Wrap around to last
        ui.move_up();
        assert_eq!(ui.state.selected(), Some(2));

        ui.move_up();
        assert_eq!(ui.state.selected(), Some(1));

        ui.move_up();
        assert_eq!(ui.state.selected(), Some(0));
    }

    #[test]
    fn test_set_selection() {
        let mut ui = setup_ui(5);
        ui.set_selection(3);
        assert_eq!(ui.state.selected(), Some(3));

        // Out of bounds should do nothing
        ui.set_selection(10);
        assert_eq!(ui.state.selected(), Some(3));
    }

    #[test]
    fn test_handle_click_within_bounds() {
        let mut ui = setup_ui(5);
        // Mock list area
        ui.list_area = Some(Rect {
            x: 5,
            y: 10,
            width: 20,
            height: 10,
        });

        // Click on the first item (y = 11 because border takes y = 10)
        let idx1 = ui.handle_click(6, 11);
        assert_eq!(idx1, Some(0));

        // Click on the third item (y = 13)
        let idx3 = ui.handle_click(15, 13);
        assert_eq!(idx3, Some(2));

        // Click outside width
        assert_eq!(ui.handle_click(30, 11), None);

        // Click on top border
        assert_eq!(ui.handle_click(6, 10), None);

        // Click below bounds
        assert_eq!(ui.handle_click(6, 25), None);
    }
}
