use crossterm::event::Event;
use std::time::{Duration, Instant};

#[derive(Eq, PartialEq)]
#[allow(dead_code)]
pub enum ShellEvent {
    Input(Event),
    Paste(String),
    ScreenResized,
}

/// State management for detecting double key presses (Ctrl+C, Esc)
#[derive(Debug)]
pub struct DoublePressState {
    pub(crate) first_press_time: Option<Instant>,
    pub(crate) press_count: u8,
    pub(crate) timeout: Duration,
}

impl DoublePressState {
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            first_press_time: None,
            press_count: 0,
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    /// Handle key press. Returns true if it's the second press within timeout
    pub fn on_pressed(&mut self) -> bool {
        let now = Instant::now();

        match self.first_press_time {
            None => {
                // First press
                self.first_press_time = Some(now);
                self.press_count = 1;
                false
            }
            Some(first_time) => {
                if now.duration_since(first_time) <= self.timeout {
                    // Second press within timeout
                    self.press_count = 2;
                    // Reset to allow immediate next sequence detection
                    self.first_press_time = None;
                    true
                } else {
                    // Timeout passed, treat as new first press
                    self.first_press_time = Some(now);
                    self.press_count = 1;
                    false
                }
            }
        }
    }

    /// Reset state
    pub fn reset(&mut self) {
        self.first_press_time = None;
        self.press_count = 0;
    }
}

pub enum SuggestionAcceptMode {
    Full,
    Word,
}
