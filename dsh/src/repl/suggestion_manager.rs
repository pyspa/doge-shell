use crate::suggestion::{InputPreferences, SuggestionEngine, SuggestionState};

pub struct SuggestionManager {
    pub engine: SuggestionEngine,
    pub active: Option<SuggestionState>,
    pub candidates: Vec<SuggestionState>,
    pub index: usize,
}

impl SuggestionManager {
    pub fn new() -> Self {
        Self {
            engine: SuggestionEngine::new(),
            active: None,
            candidates: Vec::new(),
            index: 0,
        }
    }

    pub fn set_preferences(&mut self, prefs: InputPreferences) {
        self.engine.set_preferences(prefs);
    }

    pub fn clear(&mut self) {
        self.candidates.clear();
        self.index = 0;
        self.active = None;
    }

    pub fn rotate(&mut self, step: isize) -> bool {
        let len = self.candidates.len();
        if len <= 1 {
            return false;
        }
        let len = len as isize;
        let mut next = self.index as isize + step;
        next %= len;
        if next < 0 {
            next += len;
        }
        if next as usize == self.index {
            return false;
        }
        self.index = next as usize;
        self.active = self.candidates.get(self.index).cloned();
        true
    }

    pub fn update_candidates(&mut self, mut candidates: Vec<SuggestionState>) -> bool {
        candidates.dedup_by(|a, b| a.full == b.full && a.source == b.source);
        if candidates.is_empty() {
            let had = !self.candidates.is_empty();
            self.clear();
            return had;
        }

        let changed = candidates != self.candidates;
        if changed {
            self.candidates = candidates;
            self.index = 0;
        }

        self.active = self.candidates.get(self.index).cloned();
        changed
    }

    pub fn suffix(&self, input: &str) -> Option<String> {
        let suggestion = self.active.as_ref()?;
        if !suggestion.full.starts_with(input) || suggestion.full.len() <= input.len() {
            return None;
        }
        Some(suggestion.full[input.len()..].to_string())
    }
}
