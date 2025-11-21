use crate::history::FrecencyHistory;
use dsh_frecency::SortMethod;
use dsh_openai::ChatGptClient;
use parking_lot::Mutex as ParkingMutex;
use serde_json::{Value, json};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

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

const HISTORY_CONTEXT_LIMIT: usize = 8;

#[derive(Debug, Clone)]
pub struct SuggestionRequest {
    pub input: String,
    pub cursor: usize,
    pub preferences: InputPreferences,
    pub history_context: Vec<String>,
}

impl SuggestionRequest {
    pub fn new(
        input: String,
        cursor: usize,
        preferences: InputPreferences,
        history_context: Vec<String>,
    ) -> Self {
        Self {
            input,
            cursor,
            preferences,
            history_context,
        }
    }
}

pub trait SuggestionBackend: Send + Sync {
    fn predict(&self, request: SuggestionRequest) -> Option<String>;

    fn is_pending(&self) -> bool {
        false
    }
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

    pub fn set_ai_backend(&mut self, backend: Option<Arc<dyn SuggestionBackend + Send + Sync>>) {
        self.ai_backend = backend;
    }

    pub fn ai_pending(&self) -> bool {
        self.config.preferences.ai_backfill
            && self
                .ai_backend
                .as_ref()
                .map(|backend| backend.is_pending())
                .unwrap_or(false)
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
            && let Some(state) = self.ai_suggestion(input, cursor, history)
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

    fn ai_suggestion(
        &self,
        input: &str,
        cursor: usize,
        history: Option<&Arc<ParkingMutex<FrecencyHistory>>>,
    ) -> Option<SuggestionState> {
        let backend = self.ai_backend.as_ref()?;
        let history_context = collect_history_context(history, input, HISTORY_CONTEXT_LIMIT);
        let request = SuggestionRequest::new(
            input.to_string(),
            cursor,
            self.config.preferences,
            history_context,
        );
        let completion = backend.predict(request)?;
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

fn collect_history_context(
    history: Option<&Arc<ParkingMutex<FrecencyHistory>>>,
    input: &str,
    limit: usize,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }

    let history = match history {
        Some(history) => history,
        None => return Vec::new(),
    };

    let history = match history.try_lock() {
        Some(guard) => guard,
        None => return Vec::new(),
    };

    let mut snapshot: Vec<String> = history
        .sorted(&SortMethod::Recent)
        .into_iter()
        .take(limit)
        .map(|item| item.item)
        .collect();

    if snapshot.len() < limit && !input.is_empty() {
        for item in history.sort_by_match(input) {
            if snapshot.len() >= limit {
                break;
            }
            if snapshot.iter().any(|existing| existing == &item.item) {
                continue;
            }
            snapshot.push(item.item);
        }
    }

    snapshot
}

const AI_SUGGESTION_SYSTEM_PROMPT: &str = r#"You are an inline completion engine for the doge-shell terminal. Given a user's partially typed command you must propose the most accurate continuation possible, leaning on the provided command history when relevant. Output only a single line containing the completed command. The line must:
- Start with the exact user input (do not change or reformat it).
- Append only the minimal additional characters needed to form a plausible next command.
- Contain no commentary, explanations, code fences, or markdown formatting.
- Avoid trailing whitespace or surrounding quotes.
If no meaningful continuation exists, return the user input unchanged."#;

#[derive(Clone)]
pub struct AiSuggestionBackend {
    client: Arc<ChatGptClient>,
    state: Arc<ParkingMutex<AiBackendState>>,
    settings: AiBackendSettings,
}

#[derive(Debug, Default)]
struct AiBackendState {
    cached: Option<AiCachedSuggestion>,
    inflight: bool,
    pending: Option<SuggestionRequest>,
}

#[derive(Debug, Clone)]
struct AiCachedSuggestion {
    completion: String,
    received_at: Instant,
}

#[derive(Debug, Clone)]
struct AiBackendSettings {
    cache_ttl: Duration,
    temperature: f64,
}

impl Default for AiBackendSettings {
    fn default() -> Self {
        Self {
            cache_ttl: Duration::from_secs(8),
            temperature: 0.0,
        }
    }
}

impl AiSuggestionBackend {
    pub fn new(client: ChatGptClient) -> Self {
        Self::with_settings(client, AiBackendSettings::default())
    }

    fn with_settings(client: ChatGptClient, settings: AiBackendSettings) -> Self {
        Self {
            client: Arc::new(client),
            state: Arc::new(ParkingMutex::new(AiBackendState::default())),
            settings,
        }
    }

    fn try_cached(&self, request: &SuggestionRequest) -> Option<String> {
        let state = self.state.lock();
        if let Some(cached) = &state.cached
            && cached.received_at.elapsed() <= self.settings.cache_ttl
            && cached.completion.starts_with(&request.input)
            && cached.completion.len() > request.input.len()
        {
            return Some(cached.completion.clone());
        }
        None
    }

    fn enqueue(&self, request: SuggestionRequest) {
        let mut state = self.state.lock();
        if state.inflight {
            let replace = match &state.pending {
                Some(existing) => existing.input != request.input,
                None => true,
            };
            if replace {
                state.pending = Some(request);
            }
            return;
        }

        debug!("ai suggestion backend scheduling request");
        state.inflight = true;
        drop(state);
        self.spawn_request(request);
    }

    fn spawn_request(&self, request: SuggestionRequest) {
        let backend = self.clone();
        thread::spawn(move || {
            let completion = backend.fetch_completion(&request);
            backend.handle_completion(request, completion);
        });
    }

    fn handle_completion(&self, request: SuggestionRequest, completion: Option<String>) {
        debug!(input = %request.input, "ai suggestion backend completed request");
        let next_request = {
            let mut state = self.state.lock();
            state.inflight = false;

            if let Some(completion) = completion {
                state.cached = Some(AiCachedSuggestion {
                    completion,
                    received_at: Instant::now(),
                });
                debug!("ai suggestion backend stored new completion");
            }

            state.pending.take()
        };

        if let Some(next) = next_request {
            self.enqueue(next);
        }
    }

    fn fetch_completion(&self, request: &SuggestionRequest) -> Option<String> {
        let messages = self.build_messages(request);
        let response = match self.client.send_chat_request(
            &messages,
            Some(self.settings.temperature),
            None,
            None,
        ) {
            Ok(value) => value,
            Err(err) => {
                warn!("ai suggestion request failed: {err:?}");
                return None;
            }
        };

        let content = extract_ai_message_content(&response)?;
        let normalized = sanitize_model_output(&content);
        if normalized.is_empty() {
            return None;
        }

        if normalized.starts_with(&request.input) {
            if normalized.len() > request.input.len() {
                return Some(normalized);
            }
            return None;
        }

        debug!("ai suggestion backend discarded response that did not preserve prefix");
        None
    }

    fn build_messages(&self, request: &SuggestionRequest) -> Vec<Value> {
        let user_payload = build_user_payload(request);
        vec![
            json!({"role": "system", "content": AI_SUGGESTION_SYSTEM_PROMPT}),
            json!({"role": "user", "content": user_payload}),
        ]
    }
}

impl SuggestionBackend for AiSuggestionBackend {
    fn predict(&self, request: SuggestionRequest) -> Option<String> {
        if request.input.trim().is_empty() {
            return None;
        }

        if let Some(result) = self.try_cached(&request) {
            return Some(result);
        }

        self.enqueue(request);
        None
    }

    fn is_pending(&self) -> bool {
        let state = self.state.lock();
        state.inflight || state.pending.is_some()
    }
}

fn build_user_payload(request: &SuggestionRequest) -> String {
    let mut payload = String::new();
    payload.push_str("UserInput: ");
    payload.push_str(&request.input);
    payload.push('\n');
    payload.push_str(&format!("CursorIndex: {}\n", request.cursor));
    payload.push_str(&format!(
        "SuggestionMode: {} | AiBackfill: {}\n",
        suggestion_mode_label(request.preferences.suggestion_mode),
        request.preferences.ai_backfill
    ));

    if !request.history_context.is_empty() {
        payload.push_str("RecentHistory:\n");
        for entry in &request.history_context {
            payload.push_str("- ");
            payload.push_str(entry);
            payload.push('\n');
        }
    }

    payload.push_str(
        "Return only the best single-line completion. The output must begin with the provided input and add characters at the end.",
    );
    payload
}

fn suggestion_mode_label(mode: SuggestionMode) -> &'static str {
    match mode {
        SuggestionMode::Off => "off",
        SuggestionMode::Ghost => "ghost",
    }
}

fn sanitize_model_output(raw: &str) -> String {
    let mut trimmed = raw.trim();

    if let Some(stripped) = trimmed.strip_prefix("```")
        && let Some(end) = stripped.rfind("```")
    {
        let inner = &stripped[..end];
        let inner = inner.trim();
        trimmed = inner
            .split_once('\n')
            .map(|(_, rest)| rest.trim())
            .unwrap_or(inner);
    }

    let trimmed = trimmed.trim_matches(&['"', '\'', '`'][..]);
    trimmed
        .lines()
        .next()
        .map(|line| line.trim().to_string())
        .unwrap_or_default()
}

fn extract_ai_message_content(response: &Value) -> Option<String> {
    let choice = response.get("choices")?.get(0)?;
    let message = choice.get("message")?;
    collect_text_segments(message.get("content")?)
}

fn collect_text_segments(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.to_string()),
        Value::Array(items) => {
            let mut combined = String::new();
            for item in items {
                if let Some(fragment) = collect_text_segments(item) {
                    combined.push_str(&fragment);
                }
            }
            if combined.is_empty() {
                None
            } else {
                Some(combined)
            }
        }
        Value::Object(map) => {
            if let Some(text) = map.get("text") {
                return collect_text_segments(text);
            }
            if let Some(content) = map.get("content") {
                return collect_text_segments(content);
            }
            if let Some(value_field) = map.get("value") {
                return collect_text_segments(value_field);
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    #[derive(Default)]
    struct RecordingBackend {
        calls: Mutex<Vec<SuggestionRequest>>,
        response: Mutex<Option<String>>,
    }

    impl RecordingBackend {
        fn with_response(response: &str) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                response: Mutex::new(Some(response.to_string())),
            }
        }

        fn calls(&self) -> parking_lot::MutexGuard<'_, Vec<SuggestionRequest>> {
            self.calls.lock()
        }
    }

    impl SuggestionBackend for RecordingBackend {
        fn predict(&self, request: SuggestionRequest) -> Option<String> {
            self.calls.lock().push(request);
            self.response.lock().clone()
        }
    }

    #[test]
    fn ai_backend_runs_when_preferences_enable_it() {
        let recorder = Arc::new(RecordingBackend::with_response("git status"));
        let backend: Arc<dyn SuggestionBackend + Send + Sync> = recorder.clone();

        let mut engine = SuggestionEngine::new();
        engine.set_ai_backend(Some(backend));
        engine.set_preferences(InputPreferences {
            suggestion_mode: SuggestionMode::Ghost,
            ai_backfill: true,
        });

        let result = engine.predict("git s", "git s".chars().count(), None);

        assert!(result.is_some());
        assert_eq!(result.unwrap().full, "git status");
        assert_eq!(recorder.calls().len(), 1);
    }

    #[test]
    fn suggestion_request_contains_history_snapshot() {
        let recorder = Arc::new(RecordingBackend::with_response("deploy service"));
        let backend: Arc<dyn SuggestionBackend + Send + Sync> = recorder.clone();

        let mut engine = SuggestionEngine::new();
        engine.set_ai_backend(Some(backend));
        engine.set_preferences(InputPreferences {
            suggestion_mode: SuggestionMode::Ghost,
            ai_backfill: true,
        });

        let mut history = FrecencyHistory::new();
        history.store = Some(dsh_frecency::FrecencyStore::default());
        history.add("npm run test");
        history.add("docker compose up");
        let history = Arc::new(ParkingMutex::new(history));

        let _ = engine.predict("deploy", "deploy".chars().count(), Some(&history));

        let calls = recorder.calls();
        assert!(!calls.is_empty());
        assert!(!calls[0].history_context.is_empty());
    }
}
