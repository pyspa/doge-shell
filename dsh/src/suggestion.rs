use crate::history::FrecencyHistory;
use parking_lot::Mutex as ParkingMutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SuggestionMode {
    Off,
    #[default]
    Ghost,
}

impl SuggestionMode {
    pub fn is_enabled(self) -> bool {
        matches!(self, SuggestionMode::Ghost)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputPreferences {
    pub suggestion_mode: SuggestionMode,
    pub ai_backfill: bool,
}

impl Default for InputPreferences {
    fn default() -> Self {
        Self {
            suggestion_mode: SuggestionMode::Ghost,
            ai_backfill: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionSource {
    History,
    Ai,
}

#[derive(Debug, Clone)]
pub struct SuggestionState {
    pub full: String,
    pub source: SuggestionSource,
}

#[derive(Debug, Clone)]
struct CachedSuggestion {
    prefix: String,
    state: SuggestionState,
    generated_at: Instant,
}

pub trait SuggestionBackend: Send + Sync {
    fn predict(&self, input: &str) -> Option<String>;
}

#[derive(Debug, Clone, Copy)]
pub struct SuggestionConfig {
    pub preferences: InputPreferences,
    pub history_ttl: Duration,
    pub ai_ttl: Duration,
}

impl Default for SuggestionConfig {
    fn default() -> Self {
        Self {
            preferences: InputPreferences::default(),
            history_ttl: Duration::from_millis(350),
            ai_ttl: Duration::from_secs(2),
        }
    }
}

pub struct SuggestionEngine {
    config: SuggestionConfig,
    cache: Option<CachedSuggestion>,
    ai_backend: Option<Arc<dyn SuggestionBackend + Send + Sync>>,
}

impl SuggestionEngine {
    pub fn new() -> Self {
        Self {
            config: SuggestionConfig::default(),
            cache: None,
            ai_backend: None,
        }
    }

    pub fn set_preferences(&mut self, prefs: InputPreferences) {
        self.config.preferences = prefs;
        if !prefs.suggestion_mode.is_enabled() {
            self.cache = None;
        }
    }

    #[allow(dead_code)]
    pub fn set_ai_backend(&mut self, backend: Option<Arc<dyn SuggestionBackend + Send + Sync>>) {
        self.ai_backend = backend;
    }

    pub fn predict(
        &mut self,
        input: &str,
        cursor: usize,
        history: Option<&Arc<ParkingMutex<FrecencyHistory>>>,
    ) -> Option<SuggestionState> {
        if !self.config.preferences.suggestion_mode.is_enabled() {
            self.cache = None;
            return None;
        }

        if input.is_empty() {
            self.cache = None;
            return None;
        }

        let char_len = input.chars().count();
        if cursor != char_len {
            self.cache = None;
            return None;
        }

        if let Some(cached) = &self.cache
            && cached.prefix == input
            && cached.state.full.starts_with(input)
            && cached.state.full.len() > input.len()
            && cached.generated_at.elapsed() <= self.ttl_for(cached.state.source)
        {
            return Some(cached.state.clone());
        }

        if let Some(state) = self.history_suggestion(input, history) {
            self.cache = Some(CachedSuggestion {
                prefix: input.to_string(),
                state: state.clone(),
                generated_at: Instant::now(),
            });
            return Some(state);
        }

        if self.config.preferences.ai_backfill
            && let Some(state) = self.ai_suggestion(input)
        {
            self.cache = Some(CachedSuggestion {
                prefix: input.to_string(),
                state: state.clone(),
                generated_at: Instant::now(),
            });
            return Some(state);
        }

        self.cache = None;
        None
    }

    fn history_suggestion(
        &self,
        input: &str,
        history: Option<&Arc<ParkingMutex<FrecencyHistory>>>,
    ) -> Option<SuggestionState> {
        let history = history?;
        let history = history.try_lock()?;
        let entry = history.search_prefix(input)?;
        if entry.len() <= input.len() {
            return None;
        }
        Some(SuggestionState {
            full: entry,
            source: SuggestionSource::History,
        })
    }

    fn ai_suggestion(&self, input: &str) -> Option<SuggestionState> {
        let backend = self.ai_backend.as_ref()?;
        let completion = backend.predict(input)?;
        if completion.len() <= input.len() || !completion.starts_with(input) {
            return None;
        }
        Some(SuggestionState {
            full: completion,
            source: SuggestionSource::Ai,
        })
    }

    fn ttl_for(&self, source: SuggestionSource) -> Duration {
        match source {
            SuggestionSource::History => self.config.history_ttl,
            SuggestionSource::Ai => self.config.ai_ttl,
        }
    }
}
