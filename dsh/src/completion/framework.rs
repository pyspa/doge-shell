use super::display::{Candidate, CompletionConfig, CompletionDisplay};
use super::ui::{CompletionInteraction, CompletionOutcome, TerminalEventSource};
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
    Skim,
}

/// Trait implemented by completion presentation backends.
pub trait CompletionFramework {
    fn select(&self, request: CompletionRequest<'_>) -> Option<String>;
}

/// Terminal-native grid renderer backed by [`CompletionDisplay`].
#[derive(Debug, Default)]
pub struct InlineCompletionFramework;

impl CompletionFramework for InlineCompletionFramework {
    fn select(&self, request: CompletionRequest<'_>) -> Option<String> {
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
            Ok(CompletionOutcome::Submitted(value)) => Some(value),
            Ok(CompletionOutcome::Input(value)) => {
                Some(super::last_word(input_text).to_owned() + &value)
            }
            Ok(CompletionOutcome::Cancelled) | Ok(CompletionOutcome::NoSelection) => None,
            Err(error) => {
                warn!("Completion interaction failed: {}", error);
                let _ = display.clear_display();
                let _ = execute!(stdout(), cursor::Show);
                None
            }
        }
    }
}

/// Fuzzy finder UI powered by the `skim` crate.
#[derive(Debug, Default)]
pub struct SkimCompletionFramework;

impl SkimCompletionFramework {
    fn run_with_skim(items: Vec<Candidate>, query: Option<&str>) -> Option<String> {
        if items.len() == 1 {
            return Some(items[0].output().to_string());
        }

        let options = SkimOptionsBuilder::default()
            .select_1(true)
            .bind(vec!["Enter:accept".to_string()])
            .query(query.map(|s| s.to_string()))
            .build()
            .unwrap();

        let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
        for item in items {
            let _ = tx_item.send(Arc::new(item));
        }
        drop(tx_item);

        let selected = Skim::run_with(&options, Some(rx_item))
            .map(|out| match out.final_key {
                Key::Enter => out.selected_items,
                _ => Vec::new(),
            })
            .unwrap_or_default();

        selected
            .first()
            .map(|candidate| candidate.output().to_string())
    }
}

impl CompletionFramework for SkimCompletionFramework {
    fn select(&self, request: CompletionRequest<'_>) -> Option<String> {
        debug!(
            "SkimCompletionFramework selecting from {} candidates (query={:?})",
            request.items.len(),
            request.query
        );
        for (index, item) in request.items.iter().enumerate() {
            debug!("Skim candidate {}: {:?}", index, item);
        }

        Self::run_with_skim(request.items, request.query)
    }
}

pub fn select_with_framework_kind(
    kind: CompletionFrameworkKind,
    request: CompletionRequest<'_>,
) -> Option<String> {
    match kind {
        CompletionFrameworkKind::Inline => InlineCompletionFramework.select(request),
        CompletionFrameworkKind::Skim => SkimCompletionFramework.select(request),
    }
}
