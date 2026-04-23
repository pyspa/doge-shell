use super::display::{Candidate, CompletionConfig, CompletionDisplay};
use super::ui::{CompletionInteraction, CompletionOutcome, CompletionUi, TerminalEventSource};
use crossterm::{cursor, execute};
use skim::prelude::*;
use skim::{Skim, SkimItemReceiver, SkimItemSender};
use std::io::stdout;
use std::sync::Arc;
use tracing::{debug, warn};

/// Common parameters required to render completion candidates.
#[derive(Debug)]
pub struct CompletionRequest<'a> {
    pub items: Vec<Candidate>,
    pub query: Option<&'a str>,
    pub prompt_text: &'a str,
    pub input_text: &'a str,
    pub config: CompletionConfig,
}

impl<'a> CompletionRequest<'a> {
    pub fn new(
        items: Vec<Candidate>,
        query: Option<&'a str>,
        prompt_text: &'a str,
        input_text: &'a str,
        config: CompletionConfig,
    ) -> Self {
        Self {
            items,
            query,
            prompt_text,
            input_text,
            config,
        }
    }
}

/// Rendering backends available for completion selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionFrameworkKind {
    Inline,
    Floating,
    Skim,
}

/// Trait implemented by completion presentation backends.
/// Result of a completion selection attempt.
#[derive(Debug, PartialEq)]
pub enum CompletionSelection {
    Selected(String),
    None,
    Interactive(Vec<Candidate>, Option<String>),
}

/// Trait implemented by completion presentation backends.
pub trait CompletionFramework {
    fn select(&self, request: CompletionRequest<'_>) -> CompletionSelection;
}

/// Terminal-native grid renderer backed by [`CompletionDisplay`].
#[derive(Debug, Default)]
pub struct InlineCompletionFramework;

impl CompletionFramework for InlineCompletionFramework {
    fn select(&self, request: CompletionRequest<'_>) -> CompletionSelection {
        let CompletionRequest {
            items,
            prompt_text,
            input_text,
            config,
            ..
        } = request;

        let mut display =
            CompletionDisplay::new_with_config(items, prompt_text, input_text, config);
        let mut controller = CompletionInteraction::new(TerminalEventSource);

        match controller.run(&mut display) {
            Ok(CompletionOutcome::Submitted(value)) => CompletionSelection::Selected(value),
            Ok(CompletionOutcome::Input(value)) => {
                CompletionSelection::Selected(super::last_word(input_text).to_owned() + &value)
            }
            Ok(CompletionOutcome::Cancelled) | Ok(CompletionOutcome::NoSelection) => {
                CompletionSelection::None
            }
            Err(error) => {
                warn!("Completion interaction failed: {}", error);
                let _ = display.clear_display();
                let _ = execute!(stdout(), cursor::Show);
                CompletionSelection::None
            }
        }
    }
}

/// TUI grid renderer backed by ratatui and [`RatatuiCompletionUi`].
#[derive(Debug, Default)]
pub struct FloatingCompletionFramework;

impl CompletionFramework for FloatingCompletionFramework {
    fn select(&self, request: CompletionRequest<'_>) -> CompletionSelection {
        let CompletionRequest {
            items,
            config,
            input_text,
            query,
            ..
        } = request;

        let fallback_items = items.clone();
        let fallback_query = query.map(|s| s.to_string());
        let mut display = crate::completion::floating::RatatuiCompletionUi::new(items, config);
        let mut controller = CompletionInteraction::new(TerminalEventSource);

        match controller.run(&mut display) {
            Ok(CompletionOutcome::Submitted(value)) => CompletionSelection::Selected(value),
            Ok(CompletionOutcome::Input(value)) => {
                CompletionSelection::Selected(super::last_word(input_text).to_owned() + &value)
            }
            Ok(CompletionOutcome::Cancelled) | Ok(CompletionOutcome::NoSelection) => {
                CompletionSelection::None
            }
            Err(error) => {
                warn!("Floating completion interaction failed: {}", error);
                let _ = display.clear();
                let _ = execute!(stdout(), cursor::Show);
                warn!("Falling back to skim interactive completion");
                CompletionSelection::Interactive(fallback_items, fallback_query)
            }
        }
    }
}

/// Fuzzy finder UI powered by the `skim` crate.
#[derive(Debug, Default)]
pub struct SkimCompletionFramework;

impl SkimCompletionFramework {
    pub fn run_with_skim(items: Vec<Candidate>, query: Option<String>) -> Option<String> {
        if items.len() == 1 {
            return Some(items[0].output().to_string());
        }

        // Spawn a separate thread to run Skim, isolating it from the tokio runtime
        // This prevents "Cannot start a runtime from within a runtime" panics
        std::thread::spawn(move || {
            let mut options_builder = SkimOptionsBuilder::default();
            options_builder.select_1(true);
            options_builder.bind(vec!["Enter:accept".to_string()]);
            if let Some(query) = query {
                options_builder.query(query);
            }

            let options = match options_builder.build() {
                Ok(options) => options,
                Err(err) => {
                    warn!("Failed to build skim options: {}", err);
                    return None;
                }
            };

            let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
            for item in items {
                let _ = tx_item.send(vec![Arc::new(item)]);
            }
            drop(tx_item);

            let selected = Skim::run_with(options, Some(rx_item))
                .ok()
                .map(|out| {
                    if out.is_abort {
                        Vec::new()
                    } else {
                        out.selected_items
                    }
                })
                .unwrap_or_default();

            selected
                .first()
                .map(|candidate| candidate.output().to_string())
        })
        .join()
        .unwrap_or_else(|e| {
            warn!("Skim thread panicked: {:?}", e);
            None
        })
    }
}

impl CompletionFramework for SkimCompletionFramework {
    fn select(&self, request: CompletionRequest<'_>) -> CompletionSelection {
        debug!(
            "SkimCompletionFramework selecting from {} candidates (query={:?})",
            request.items.len(),
            request.query
        );
        CompletionSelection::Interactive(request.items, request.query.map(|s| s.to_string()))
    }
}

pub fn select_with_framework_kind(
    kind: CompletionFrameworkKind,
    request: CompletionRequest<'_>,
) -> CompletionSelection {
    match kind {
        CompletionFrameworkKind::Inline => InlineCompletionFramework.select(request),
        CompletionFrameworkKind::Floating => FloatingCompletionFramework.select(request),
        CompletionFrameworkKind::Skim => SkimCompletionFramework.select(request),
    }
}
