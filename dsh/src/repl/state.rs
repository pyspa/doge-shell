use crossterm::event::Event;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) struct ReplState {
    pub should_exit: bool,
    pub last_command_time: Option<Instant>,
    pub last_duration: Option<Duration>,
    pub last_status: i32,
    pub last_command_string: String,
    pub stopped_jobs_warned: bool,
    pub multiline_buffer: String,
    pub last_cwd: PathBuf,
}

impl ReplState {
    pub fn new(current_dir: PathBuf) -> Self {
        Self {
            should_exit: false,
            last_command_time: None,
            last_duration: None,
            last_status: 0,
            last_command_string: String::new(),
            stopped_jobs_warned: false,
            multiline_buffer: String::new(),
            last_cwd: current_dir,
        }
    }
}

#[derive(Eq, PartialEq)]
pub enum ShellEvent {
    Input(Event),
    // Planned event variant; not emitted yet. Terminal resizes arrive as
    // `Input(Event::Resize(..))` from the crossterm EventStream.
    #[allow(dead_code)]
    Paste(String),
}

#[derive(Debug)]
pub enum InteractiveAction {
    Patch {
        backspace_count: usize,
        text: String,
    },
    ReplaceRange {
        start: usize,
        end: usize,
        text: String,
    },
    ReplaceAll {
        text: String,
    },
    /// Replace the buffer and run it immediately.
    ///
    /// A `RunInteractive` closure is synchronous and cannot execute anything
    /// itself, so a full-screen UI that wants to act — re-run a block, `cd` to
    /// where it ran, ask the AI to explain it — hands the command back this way.
    ReplaceAllAndExecute {
        text: String,
    },
}

pub enum ReplControlFlow {
    Continue,
    RunInteractive(Box<dyn FnOnce() -> anyhow::Result<Option<InteractiveAction>> + Send>),
    ExecuteCurrentInput,
    OpenCommandPalette,
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
