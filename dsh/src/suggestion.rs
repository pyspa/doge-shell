use crate::history::FrecencyHistory;
use dsh_frecency::SortMethod;
use dsh_openai::ChatGptClient;
use parking_lot::Mutex as ParkingMutex;
use serde_json::{Value, json};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::Notify;
use tokio::task;
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
    pub transient_prompt: bool,
}

impl Default for InputPreferences {
    fn default() -> Self {
        Self {
            suggestion_mode: SuggestionMode::Ghost,
            ai_backfill: false,
            transient_prompt: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionSource {
    History,
    Ai,
    Completion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    history_cache: Option<CachedSuggestion>,
    ai_cache: Option<CachedSuggestion>,
    ai_backend: Option<Arc<dyn SuggestionBackend + Send + Sync>>,
}

impl Default for SuggestionEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SuggestionEngine {
    pub fn new() -> Self {
        Self {
            config: SuggestionConfig::default(),
            history_cache: None,
            ai_cache: None,
            ai_backend: None,
        }
    }

    pub fn set_preferences(&mut self, prefs: InputPreferences) {
        self.config.preferences = prefs;
        if !prefs.suggestion_mode.is_enabled() {
            self.history_cache = None;
            self.ai_cache = None;
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
    ) -> Vec<SuggestionState> {
        if !self.config.preferences.suggestion_mode.is_enabled() {
            self.history_cache = None;
            self.ai_cache = None;
            return Vec::new();
        }

        if input.is_empty() {
            self.history_cache = None;
            self.ai_cache = None;
            return Vec::new();
        }

        let char_len = input.chars().count();
        if cursor > char_len {
            return Vec::new();
        }

        let mut suggestions = Vec::new();

        if let Some(state) = self.use_cache(self.history_cache.as_ref(), input) {
            suggestions.push(state.clone());
        } else if let Some(state) = self.history_suggestion(input, history) {
            self.history_cache = Some(CachedSuggestion {
                prefix: input.to_string(),
                state: state.clone(),
                generated_at: Instant::now(),
            });
            suggestions.push(state);
        } else {
            self.history_cache = None;
        }

        if self.config.preferences.ai_backfill {
            if let Some(state) = self.use_cache(self.ai_cache.as_ref(), input) {
                suggestions.push(state.clone());
            }

            if let Some(state) = self.ai_suggestion(input, cursor, history) {
                let duplicate = suggestions
                    .iter()
                    .any(|existing| existing.full == state.full && existing.source == state.source);
                if !duplicate {
                    suggestions.push(state.clone());
                }
                self.ai_cache = Some(CachedSuggestion {
                    prefix: input.to_string(),
                    state,
                    generated_at: Instant::now(),
                });
            }
        } else {
            self.ai_cache = None;
        }

        suggestions
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

    fn use_cache(&self, cache: Option<&CachedSuggestion>, input: &str) -> Option<SuggestionState> {
        let cached = cache?;
        if cached.prefix == input
            && cached.state.full.starts_with(input)
            && cached.state.full.len() > input.len()
            && cached.generated_at.elapsed() <= self.ttl_for(cached.state.source)
        {
            return Some(cached.state.clone());
        }
        None
    }

    fn ttl_for(&self, source: SuggestionSource) -> Duration {
        match source {
            SuggestionSource::History => self.config.history_ttl,
            SuggestionSource::Ai => self.config.ai_ttl,
            SuggestionSource::Completion => self.config.history_ttl,
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

    // Pre-allocate with expected capacity
    let mut snapshot: Vec<String> = Vec::with_capacity(limit);

    // 1. Add recent items (context of "what I am doing now")
    // Take half of the limit for recent items
    let recent_limit = (limit as f32 * 0.5).ceil() as usize;
    snapshot.extend(
        history
            .sorted(&SortMethod::Recent)
            .into_iter()
            .take(recent_limit)
            .map(|item| item.item),
    );

    // 2. Add frecent items (context of "what I usually do")
    // Fill the rest with frecent items, avoiding duplicates
    let frecent_items = history.sorted(&SortMethod::Frecent);
    for item in frecent_items {
        if snapshot.len() >= limit {
            break;
        }
        if !snapshot.contains(&item.item) {
            snapshot.push(item.item);
        }
    }

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
If no meaningful continuation exists, return the user input unchanged.
Return only the best single-line completion. The output must begin with the provided input and add characters at the end."#;

#[derive(Clone)]
pub struct AiSuggestionBackend {
    inner: Arc<AiBackendInner>,
}

struct AiBackendInner {
    client: Arc<ChatGptClient>,
    state: ParkingMutex<AiBackendState>,
    settings: AiBackendSettings,
    notify: Notify,
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
        let inner = Arc::new(AiBackendInner {
            client: Arc::new(client),
            state: ParkingMutex::new(AiBackendState::default()),
            settings,
            notify: Notify::new(),
        });
        let backend = Self {
            inner: inner.clone(),
        };
        backend.spawn_worker(inner);
        backend
    }

    fn spawn_worker(&self, inner: Arc<AiBackendInner>) {
        let runner = self.clone_with_inner(inner);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                runner.worker_loop().await;
            });
        } else {
            thread::spawn(move || {
                let runtime = match Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => {
                        warn!("Failed to create runtime for AI backend: {}", e);
                        return; // Exit thread without panic
                    }
                };
                runtime.block_on(async move {
                    runner.worker_loop().await;
                });
            });
        }
    }

    fn clone_with_inner(&self, inner: Arc<AiBackendInner>) -> Self {
        Self { inner }
    }

    async fn worker_loop(self) {
        loop {
            let request = self.next_request().await;
            let completion = self.fetch_completion_async(&request).await;
            self.handle_completion(request, completion);
        }
    }

    async fn next_request(&self) -> SuggestionRequest {
        loop {
            if let Some(request) = {
                let mut state = self.inner.state.lock();
                if let Some(req) = state.pending.take() {
                    state.inflight = true;
                    Some(req)
                } else {
                    state.inflight = false;
                    None
                }
            } {
                return request;
            }

            self.inner.notify.notified().await;
        }
    }

    async fn fetch_completion_async(&self, request: &SuggestionRequest) -> Option<String> {
        let backend = self.clone();
        let request = request.clone();
        task::spawn_blocking(move || backend.fetch_completion(&request))
            .await
            .ok()
            .flatten()
    }

    fn try_cached(&self, request: &SuggestionRequest) -> Option<String> {
        let state = self.inner.state.lock();
        if let Some(cached) = &state.cached
            && cached.received_at.elapsed() <= self.inner.settings.cache_ttl
            && cached.completion.starts_with(&request.input)
            && cached.completion.len() > request.input.len()
        {
            return Some(cached.completion.clone());
        }
        None
    }

    fn enqueue(&self, request: SuggestionRequest) {
        let mut state = self.inner.state.lock();
        let replace = match &state.pending {
            Some(existing) => existing.input != request.input,
            None => true,
        };
        if replace {
            state.pending = Some(request);
        }
        if state.inflight {
            return;
        }
        state.inflight = true;
        drop(state);
        self.inner.notify.notify_one();
    }

    fn handle_completion(&self, request: SuggestionRequest, completion: Option<String>) {
        debug!(input = %request.input, "ai suggestion backend completed request");
        let mut state = self.inner.state.lock();
        if let Some(completion) = completion {
            state.cached = Some(AiCachedSuggestion {
                completion,
                received_at: Instant::now(),
            });
            debug!("ai suggestion backend stored new completion");
        }

        if state.pending.is_some() {
            state.inflight = true;
            self.inner.notify.notify_one();
        } else {
            state.inflight = false;
        }
    }

    fn fetch_completion(&self, request: &SuggestionRequest) -> Option<String> {
        let messages = self.build_messages(request);
        let response = match self.inner.client.send_chat_request(
            &messages,
            Some(self.inner.settings.temperature),
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
        let state = self.inner.state.lock();
        state.inflight || state.pending.is_some()
    }
}

fn build_user_payload(request: &SuggestionRequest) -> String {
    let mut payload = String::new();

    // 1. History (Semi-Static)
    // Placed first to maximize prefix caching when typing adds characters to Input
    if !request.history_context.is_empty() {
        payload.push_str("RecentHistory:\n");
        for entry in &request.history_context {
            payload.push_str("- ");
            payload.push_str(entry);
            payload.push('\n');
        }
    }

    // 2. Mode (Static/Semi-static)
    payload.push_str(&format!(
        "SuggestionMode: {} | AiBackfill: {}\n",
        suggestion_mode_label(request.preferences.suggestion_mode),
        request.preferences.ai_backfill
    ));

    // 3. UserInput (Dynamic)
    payload.push_str("UserInput: ");
    payload.push_str(&request.input);
    payload.push('\n');

    // 4. Cursor (Dynamic)
    payload.push_str(&format!("CursorIndex: {}\n", request.cursor));

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
            transient_prompt: true,
        });

        let result = engine.predict("git s", "git s".chars().count(), None);

        assert!(!result.is_empty());
        assert_eq!(result[0].full, "git status");
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
            transient_prompt: true,
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

    #[test]
    fn test_prompt_structure() {
        // Use verify that History comes BEFORE Input in build_user_payload output
        // We can't access build_user_payload directly as it is private, but we can infer from the recorded request
        // Actually, RecordingBackend stores SuggestionRequest, not the JSON payload.
        // build_user_payload is called inside fetch_completion (private).

        // Use a test-only public wrapper or expose build_user_payload for tests?
        // Or just trust the code I wrote?
        // Ideally I should test `build_user_payload`.
        // Let's modify `suggestion.rs` to make `build_user_payload` visible for tests.

        let request = SuggestionRequest {
            input: "git c".to_string(),
            cursor: 5,
            preferences: InputPreferences::default(),
            history_context: vec!["git status".to_string(), "git checkout".to_string()],
        };

        let payload = build_user_payload(&request);

        let history_idx = payload.find("RecentHistory:");
        let input_idx = payload.find("UserInput:");

        assert!(history_idx.is_some());
        assert!(input_idx.is_some());
        // History must come BEFORE Input
        assert!(history_idx.unwrap() < input_idx.unwrap());
    }
}
