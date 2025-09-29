use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tracing::debug;

/// Abstraction over the display implementation so it can be tested
pub trait CompletionUi {
    fn show(&mut self) -> Result<()>;
    fn refresh_selection(&mut self) -> Result<()>;
    fn clear(&mut self) -> Result<()>;
    fn move_up(&mut self);
    fn move_down(&mut self);
    fn move_left(&mut self);
    fn move_right(&mut self);
    fn selected_output(&self) -> Option<String>;
}

/// Source of input events (terminal during runtime, stub during tests)
pub trait CompletionEventSource {
    fn next_event(&mut self) -> Result<Event>;
}

/// Default terminal-backed event source
#[derive(Default)]
pub struct TerminalEventSource;

impl CompletionEventSource for TerminalEventSource {
    fn next_event(&mut self) -> Result<Event> {
        Ok(event::read()?)
    }
}

/// Result of the completion interaction loop
#[derive(Debug, PartialEq, Eq)]
pub enum CompletionOutcome {
    Submitted(String),
    Input(String),
    Cancelled,
    NoSelection,
}

/// Controller that drives the completion UI using an event source
pub struct CompletionInteraction<E> {
    event_source: E,
}

impl<E> CompletionInteraction<E> {
    pub fn new(event_source: E) -> Self {
        Self { event_source }
    }
}

impl<E> CompletionInteraction<E>
where
    E: CompletionEventSource,
{
    pub fn run<U>(&mut self, ui: &mut U) -> Result<CompletionOutcome>
    where
        U: CompletionUi,
    {
        ui.show()?;

        loop {
            let event = self.event_source.next_event()?;
            match event {
                Event::Key(key_event) => match interpret_key_event(&key_event) {
                    InteractionCommand::MoveUp => {
                        ui.move_up();
                        ui.refresh_selection()?;
                    }
                    InteractionCommand::MoveDown => {
                        ui.move_down();
                        ui.refresh_selection()?;
                    }
                    InteractionCommand::MoveLeft => {
                        ui.move_left();
                        ui.refresh_selection()?;
                    }
                    InteractionCommand::MoveRight => {
                        ui.move_right();
                        ui.refresh_selection()?;
                    }
                    InteractionCommand::Submit => {
                        let selection = ui.selected_output();
                        ui.clear()?;
                        if let Some(value) = selection {
                            debug!("Completion submitted: {}", value);
                            return Ok(CompletionOutcome::Submitted(value));
                        }
                        debug!("Completion confirmed without selection");
                        return Ok(CompletionOutcome::NoSelection);
                    }
                    InteractionCommand::Input(ch) => {
                        ui.clear()?;
                        debug!("Completion cancelled by input");
                        return Ok(CompletionOutcome::Input(ch.to_string()));
                    }
                    InteractionCommand::Cancel => {
                        ui.clear()?;
                        debug!("Completion cancelled by user");
                        return Ok(CompletionOutcome::Cancelled);
                    }
                    InteractionCommand::Noop => {}
                },
                _ => {
                    debug!("Ignoring non-key event during completion interaction");
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum InteractionCommand {
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    Submit,
    Input(char),
    Cancel,
    Noop,
}

fn interpret_key_event(key: &KeyEvent) -> InteractionCommand {
    if key.kind != KeyEventKind::Press {
        return InteractionCommand::Noop;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Up, _) => InteractionCommand::MoveUp,
        (KeyCode::Down, _) => InteractionCommand::MoveDown,
        (KeyCode::Left, _) => InteractionCommand::MoveLeft,
        (KeyCode::Right, _) | (KeyCode::Tab, KeyModifiers::NONE) => InteractionCommand::MoveRight,
        (KeyCode::Enter, _) => InteractionCommand::Submit,
        (KeyCode::Esc, _)
        | (KeyCode::Char('c'), KeyModifiers::CONTROL)
        | (KeyCode::Char('g'), KeyModifiers::CONTROL)
        | (KeyCode::Char('q'), KeyModifiers::NONE) => InteractionCommand::Cancel,
        (KeyCode::Char(ch), _) => InteractionCommand::Input(ch),
        _ => InteractionCommand::Noop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    struct TestEventSource {
        events: Vec<Event>,
        index: usize,
    }

    impl TestEventSource {
        fn new(events: Vec<Event>) -> Self {
            Self { events, index: 0 }
        }
    }

    impl CompletionEventSource for TestEventSource {
        fn next_event(&mut self) -> Result<Event> {
            if let Some(event) = self.events.get(self.index) {
                self.index += 1;
                Ok(event.clone())
            } else {
                Err(anyhow!("no more events"))
            }
        }
    }

    #[derive(Default)]
    struct MockUi {
        show_calls: usize,
        refresh_calls: usize,
        clear_calls: usize,
        move_up_calls: usize,
        move_down_calls: usize,
        move_left_calls: usize,
        move_right_calls: usize,
        selected: Option<String>,
    }

    impl CompletionUi for MockUi {
        fn show(&mut self) -> Result<()> {
            self.show_calls += 1;
            Ok(())
        }

        fn refresh_selection(&mut self) -> Result<()> {
            self.refresh_calls += 1;
            Ok(())
        }

        fn clear(&mut self) -> Result<()> {
            self.clear_calls += 1;
            Ok(())
        }

        fn move_up(&mut self) {
            self.move_up_calls += 1;
        }

        fn move_down(&mut self) {
            self.move_down_calls += 1;
        }

        fn move_left(&mut self) {
            self.move_left_calls += 1;
        }

        fn move_right(&mut self) {
            self.move_right_calls += 1;
        }

        fn selected_output(&self) -> Option<String> {
            self.selected.clone()
        }
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(code: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL))
    }

    #[test]
    fn submits_selection_and_clears_ui() {
        let mut ui = MockUi {
            selected: Some("ls".to_string()),
            ..MockUi::default()
        };
        let events = vec![key(KeyCode::Enter)];
        let mut controller = CompletionInteraction::new(TestEventSource::new(events));

        let outcome = controller.run(&mut ui).expect("controller run succeeds");

        assert_eq!(outcome, CompletionOutcome::Submitted("ls".to_string()));
        assert_eq!(ui.clear_calls, 1);
        assert_eq!(ui.show_calls, 1);
    }

    #[test]
    fn cancels_on_ctrl_c() {
        let mut ui = MockUi::default();
        let events = vec![ctrl('c')];
        let mut controller = CompletionInteraction::new(TestEventSource::new(events));

        let outcome = controller.run(&mut ui).expect("controller run succeeds");

        assert_eq!(outcome, CompletionOutcome::Cancelled);
        assert_eq!(ui.clear_calls, 1);
        assert_eq!(ui.refresh_calls, 0);
    }

    #[test]
    fn moves_and_refreshes_selection() {
        let mut ui = MockUi::default();
        let events = vec![key(KeyCode::Down), key(KeyCode::Right), key(KeyCode::Enter)];
        let mut controller = CompletionInteraction::new(TestEventSource::new(events));

        ui.selected = Some("grep".to_string());
        let outcome = controller.run(&mut ui).expect("controller run succeeds");

        assert_eq!(outcome, CompletionOutcome::Submitted("grep".to_string()));
        assert_eq!(ui.move_down_calls, 1);
        assert_eq!(ui.move_right_calls, 1);
        assert_eq!(ui.refresh_calls, 2);
    }
}
