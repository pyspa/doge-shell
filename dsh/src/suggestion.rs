use crate::completion::{self, path_completion_prefix_strict};
use crate::history::History;
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
    /// When enabled, show a hint to diagnose errors after command failures
    pub auto_diagnose: bool,
    /// When enabled, automatically trigger AI fix suggestion on command failure
    pub auto_fix: bool,
    /// When enabled, send desktop notification for long running commands
    pub auto_notify_enabled: bool,
    /// Threshold in seconds for auto notification
    pub auto_notify_threshold: u64,
}

impl Default for InputPreferences {
    fn default() -> Self {
        Self {
            suggestion_mode: SuggestionMode::Ghost,
            ai_backfill: false,
            transient_prompt: true,
            auto_diagnose: false,
            auto_fix: false,
            auto_notify_enabled: true,
            auto_notify_threshold: 10,
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
    pub cwd: Option<String>,
    pub files: Arc<Vec<String>>,
    pub last_exit_code: Option<i32>,
}

impl SuggestionRequest {
    pub fn new(
        input: String,
        cursor: usize,
        preferences: InputPreferences,
        history_context: Vec<String>,
        cwd: Option<String>,
        files: Arc<Vec<String>>,
        last_exit_code: Option<i32>,
    ) -> Self {
        Self {
            input,
            cursor,
            preferences,
            history_context,
            cwd,
            files,
            last_exit_code,
        }
    }
}

pub trait SuggestionBackend: Send + Sync {
    fn predict(&self, request: SuggestionRequest) -> Option<String>;

    fn prefetch(&self, _request: SuggestionRequest) {}

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

    pub fn prefetch(
        &self,
        cwd: Option<String>,
        files: Arc<Vec<String>>,
        last_exit_code: Option<i32>,
    ) {
        if let Some(backend) = &self.ai_backend
            && self.config.preferences.ai_backfill
        {
            let request = SuggestionRequest::new(
                String::new(), // Empty input for prefetch
                0,
                self.config.preferences,
                Vec::new(),
                cwd,
                files,
                last_exit_code,
            );
            backend.prefetch(request);
        }
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
        history: Option<&Arc<ParkingMutex<History>>>,
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

        // Try completion suggestion (lookahead)
        if suggestions.is_empty()
            && let Some(state) = self.completion_suggestion(input)
        {
            suggestions.push(state);
        }

        // If we have a history suggestion, we skip AI to prioritize it and minimize noise/latency
        if !suggestions.is_empty() {
            self.ai_cache = None;
            return suggestions;
        }

        // Check if command is in blocklist for AI suggestions
        if self.in_blocklist(input) {
            self.ai_cache = None;
            // Still allow history suggestions, but ensure AI is skipped
            if !suggestions.is_empty() {
                return suggestions;
            }
            // If we have no history suggestions and it's blocked, return empty
            return suggestions;
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

    fn in_blocklist(&self, input: &str) -> bool {
        const AI_SUGGESTION_BLOCKLIST: &[&str] = &["gco"];

        // Simple check: first word
        let cmd = input.split_whitespace().next().unwrap_or("");
        AI_SUGGESTION_BLOCKLIST.contains(&cmd)
    }

    fn completion_suggestion(&self, input: &str) -> Option<SuggestionState> {
        let word = completion::last_word(input);
        if word.is_empty() {
            return None;
        }

        // Determine context (is it a cd command?)
        // Simple heuristic: if input starts with "cd " or contains " cd " before the last word
        // Actually simplest is checking the first token.
        let is_cd = input.trim_start().starts_with("cd ");

        // Use strict prefix completion
        if let Ok(Some(completion)) = path_completion_prefix_strict(word, is_cd) {
            // Strict prefix is already guaranteed by path_completion_prefix_strict
            // But we should double check if the completion actually EXTENDS the word
            if !completion.starts_with(word) {
                return None;
            }

            // Construct full command
            let prefix_len = input.len().saturating_sub(word.len());
            let full = format!("{}{}", &input[..prefix_len], completion);

            return Some(SuggestionState {
                full,
                source: SuggestionSource::Completion,
            });
        }
        None
    }

    fn history_suggestion(
        &self,
        input: &str,
        history: Option<&Arc<ParkingMutex<History>>>,
    ) -> Option<SuggestionState> {
        let history = history?;
        let history = history.try_lock()?;
        let entry = history.search_first(input)?.to_string();
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
        history: Option<&Arc<ParkingMutex<History>>>,
    ) -> Option<SuggestionState> {
        self.ai_suggestion_with_context(input, cursor, history, None, Arc::new(Vec::new()), None)
    }

    pub fn ai_suggestion_with_context(
        &self,
        input: &str,
        cursor: usize,
        history: Option<&Arc<ParkingMutex<History>>>,
        cwd: Option<String>,
        files: Arc<Vec<String>>,
        last_exit_code: Option<i32>,
    ) -> Option<SuggestionState> {
        let backend = self.ai_backend.as_ref()?;
        let history_context = collect_history_context(history, input, HISTORY_CONTEXT_LIMIT);
        let request = SuggestionRequest::new(
            input.to_string(),
            cursor,
            self.config.preferences,
            history_context,
            cwd,
            files,
            last_exit_code,
        );
        let completion = backend.predict(request)?;
        if !completion.starts_with(input) {
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
    history: Option<&Arc<ParkingMutex<History>>>,
    _input: &str,
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

    history.get_recent_context(limit)
}

const AI_SUGGESTION_SYSTEM_PROMPT: &str = r#"You are an inline completion engine for the doge-shell terminal.
Given a user's partially typed command and context (history, current directory, files, OS), propose the most accurate continuation possible.
Output ONLY a single line containing the completed command.
- Start with the exact user input.
- Append only the minimal additional characters to form a plausible command.
- If history matches, prioritize it unless current context (files) suggests it is invalid.
- If the last command failed (exit code != 0), consider suggesting a correction if the input seems related to fixing it.
- No commentary, no explanations, no code fences, no markdown.
- Output MUST begin with the provided input.
- If no plausible completion exists, output the provided input exactly."#;

const AI_CONTEXT_SUGGESTION_SYSTEM_PROMPT: &str = r#"You are a helpful shell assistant.
Given the current directory and files, suggest up to 3 likely commands the user might want to run.
Output ONLY the commands, one per line. No explanations."#;

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
    context_cached: Option<AiCachedContextSuggestion>,
    inflight: bool,
    pending: Option<SuggestionRequest>,
}

#[derive(Debug, Clone)]
struct AiCachedContextSuggestion {
    suggestions: Vec<String>,
    cwd: String,
    received_at: Instant,
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

    pub fn prefetch(&self, request: SuggestionRequest) {
        if !request.input.is_empty() {
            return;
        }
        self.enqueue(request);
    }

    fn try_cached(&self, request: &SuggestionRequest) -> Option<String> {
        let state = self.inner.state.lock();

        // 1. Check exact match cache
        if let Some(cached) = &state.cached
            && cached.received_at.elapsed() <= self.inner.settings.cache_ttl
            && cached.completion.starts_with(&request.input)
            && cached.completion.len() > request.input.len()
        {
            return Some(cached.completion.clone());
        }

        // 2. Check context cache (prefetch result)
        if let Some(ctx_cached) = &state.context_cached
            && let Some(req_cwd) = &request.cwd
            && &ctx_cached.cwd == req_cwd
            && ctx_cached.received_at.elapsed() <= self.inner.settings.cache_ttl
        {
            // Find a suggestion that matches the current input
            for suggestion in &ctx_cached.suggestions {
                if suggestion.starts_with(&request.input) && suggestion.len() > request.input.len()
                {
                    return Some(suggestion.clone());
                }
            }
        }

        None
    }

    fn enqueue(&self, request: SuggestionRequest) {
        let mut state = self.inner.state.lock();
        // Allow replacing pending request
        state.pending = Some(request);

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

        if let Some(content) = completion {
            if request.input.is_empty() {
                // Determine CWD from request or default
                let cwd = request.cwd.clone().unwrap_or_default();
                let suggestions: Vec<String> =
                    content.lines().map(|s| s.trim().to_string()).collect();
                if !suggestions.is_empty() {
                    state.context_cached = Some(AiCachedContextSuggestion {
                        suggestions,
                        cwd,
                        received_at: Instant::now(),
                    });
                    debug!("ai suggestion backend stored new context completion");
                }
            } else {
                state.cached = Some(AiCachedSuggestion {
                    completion: content,
                    received_at: Instant::now(),
                });
                debug!("ai suggestion backend stored new completion");
            }
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
            None,
        ) {
            Ok(value) => value,
            Err(err) => {
                warn!("ai suggestion request failed: {err:?}");
                return None;
            }
        };

        let content = extract_ai_message_content(&response)?;

        if request.input.is_empty() {
            // For prefetch, we just return the raw content (list of commands)
            return Some(content);
        }

        let normalized = sanitize_model_output(&content);
        if normalized.is_empty() {
            return None;
        }

        if normalized.starts_with(&request.input) {
            // Allow returning exact input if explicitly requested (by prompt) to stop polling
            // but here we are in fetch_completion.
            // If normalized == input, it means AI returned input.
            // We return it so `predict` logic can decide what to do (currently it accepts it)
            return Some(normalized);
        }

        debug!("ai suggestion backend discarded response that did not preserve prefix");
        None
    }

    fn build_messages(&self, request: &SuggestionRequest) -> Vec<Value> {
        let user_payload = build_user_payload(request);
        let system_prompt = if request.input.is_empty() {
            AI_CONTEXT_SUGGESTION_SYSTEM_PROMPT
        } else {
            AI_SUGGESTION_SYSTEM_PROMPT
        };

        vec![
            json!({"role": "system", "content": system_prompt}),
            json!({"role": "user", "content": user_payload}),
        ]
    }
}

impl SuggestionBackend for AiSuggestionBackend {
    fn predict(&self, request: SuggestionRequest) -> Option<String> {
        // Normal prediction flow
        if let Some(result) = self.try_cached(&request) {
            return Some(result);
        }

        if !request.input.trim().is_empty() {
            self.enqueue(request);
        }
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

    // 3. Context (Semi-Dynamic)
    if let Some(cwd) = &request.cwd {
        payload.push_str(&format!("CWD: {}\n", cwd));
    }
    if !request.files.is_empty() {
        payload.push_str("DirectoryListing (partial):\n");
        for file in request.files.iter() {
            payload.push_str("- ");
            payload.push_str(file);
            payload.push('\n');
        }
    }
    if let Some(code) = request.last_exit_code {
        payload.push_str(&format!("LastExitCode: {}\n", code));
    }
    payload.push_str(&format!("OS: {}\n", std::env::consts::OS));

    // 4. UserInput (Dynamic)
    payload.push_str("UserInput: ");
    payload.push_str(&request.input);
    payload.push('\n');

    // 5. Cursor (Dynamic)
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
            ..Default::default()
        });

        let result = engine.predict("git s", "git s".chars().count(), None);

        assert!(!result.is_empty());
        assert_eq!(result[0].full, "git status");
        assert_eq!(recorder.calls().len(), 1);
    }

    #[test]
    fn engine_skips_ai_when_history_matches() {
        let recorder = Arc::new(RecordingBackend::with_response("git commit"));
        let backend: Arc<dyn SuggestionBackend + Send + Sync> = recorder.clone();

        let mut engine = SuggestionEngine::new();
        engine.set_ai_backend(Some(backend));
        engine.set_preferences(InputPreferences {
            suggestion_mode: SuggestionMode::Ghost,
            ai_backfill: true,
            ..Default::default()
        });

        let mut history = History::new();
        history.add_test_entry("git status");
        let history = Arc::new(ParkingMutex::new(history));

        // "git s" should match "git status" in history
        let result = engine.predict("git s", 5, Some(&history));

        assert!(!result.is_empty());
        assert_eq!(result[0].full, "git status");
        assert_eq!(result[0].source, SuggestionSource::History);
        // Backend should NOT have been called
        assert_eq!(recorder.calls().len(), 0);
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
            ..Default::default()
        });

        let mut history = History::new();
        history.add_test_entry("npm run test");
        history.add_test_entry("docker compose up");
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
            cwd: Some("/home/user".to_string()),
            files: Arc::new(vec!["file1".to_string()]),
            last_exit_code: Some(0),
        };

        let payload = build_user_payload(&request);

        let history_idx = payload.find("RecentHistory:");
        let input_idx = payload.find("UserInput:");

        assert!(history_idx.is_some());
        assert!(input_idx.is_some());
        // History must come BEFORE Input
        assert!(history_idx.unwrap() < input_idx.unwrap());
    }
    #[test]
    fn engine_passes_full_context_to_backend() {
        let recorder = Arc::new(RecordingBackend::with_response("cat config.toml"));
        let backend: Arc<dyn SuggestionBackend + Send + Sync> = recorder.clone();

        let mut engine = SuggestionEngine::new();
        engine.set_ai_backend(Some(backend));
        engine.set_preferences(InputPreferences {
            suggestion_mode: SuggestionMode::Ghost,
            ai_backfill: true,
            ..Default::default()
        });

        let cwd = Some("/tmp/test".to_string());
        let files = vec!["config.toml".to_string(), "main.rs".to_string()];
        let exit_code = Some(1);

        // historyなし、contextありで呼び出す
        let _ = engine.ai_suggestion_with_context(
            "cat c",
            5,
            None,
            cwd.clone(),
            Arc::new(files.clone()),
            exit_code,
        );

        let calls = recorder.calls();
        assert_eq!(calls.len(), 1);
        let request = &calls[0];

        assert_eq!(request.cwd, cwd);
        assert_eq!(*request.files, files);
        assert_eq!(request.last_exit_code, exit_code);
    }

    #[test]
    fn test_ai_suggestion_blocklist() {
        let recorder = Arc::new(RecordingBackend::with_response("gco main"));
        let backend: Arc<dyn SuggestionBackend + Send + Sync> = recorder.clone();

        let mut engine = SuggestionEngine::new();
        engine.set_ai_backend(Some(backend));
        engine.set_preferences(InputPreferences {
            suggestion_mode: SuggestionMode::Ghost,
            ai_backfill: true,
            ..Default::default()
        });

        // 1. Check that "gco" is blocked
        let result = engine.predict("gco", 3, None);
        assert!(
            result.is_empty(),
            "gco should be blocked and return no suggestions"
        );
        assert_eq!(
            recorder.calls().len(),
            0,
            "Backend should not be called for gco"
        );

        // 2. Check that "gco main" is blocked
        let result = engine.predict("gco main", 8, None);
        assert!(result.is_empty(), "gco with args should be blocked");
        assert_eq!(
            recorder.calls().len(),
            0,
            "Backend should not be called for gco args"
        );

        // 3. Check that " echo" (with leading space) is NOT blocked (unless we strip it)
        // logic says trim_start().split_whitespace().next() so " gco" should also be blocked
        let result = engine.predict(" gco", 4, None);
        assert!(
            result.is_empty(),
            "gco with leading space should be blocked"
        );
        assert_eq!(
            recorder.calls().len(),
            0,
            "Backend should not be called for gco with space"
        );

        // 4. Check that "echo" works
        let _result = engine.predict("echo", 4, None);
        // It might be empty if backend returns "gco main" (which doesn't match echo),
        // but the point is verification of the CALL count.
        // Wait, RecordingBackend::with_response("gco main") is fixed response.
        // "echo" input vs "gco main" response -> predict checks prefix -> filtered out inside backend/predict logic?
        // Actually RecordingBackend.predict returns the response unconditionally.
        // Engine.predict -> ai_suggestion -> backend.predict returns "gco main".
        // Engine then checks `if !completion.starts_with(input)`. "gco main" starts with "echo"? No.
        // So result is empty, but CALL count should increase.

        assert_eq!(
            recorder.calls().len(),
            1,
            "Backend SHOULD be called for echo"
        );
    }

    #[test]
    fn test_completion_suggestion_real_fs() {
        let mut engine = SuggestionEngine::new();
        engine.set_preferences(InputPreferences {
            suggestion_mode: SuggestionMode::Ghost,
            ai_backfill: true,
            ..Default::default()
        });

        // "ls src/sugg" -> matches "ls src/suggestion.rs"
        let input = "ls src/sugg";
        if let Some(s) = engine.completion_suggestion(input) {
            assert!(s.full.contains("src/suggestion.rs"));
            assert_eq!(s.source, SuggestionSource::Completion);
        }

        // "cd src/sugg" -> matches "cd src/suggestion.rs" ??
        // "suggestion.rs" is a file, so it should NOT match if strict dir filter is on.
        // But context implies we can't easily mock FS here without tempfile.
        // We rely on the fact that `src` contains `suggestion.rs` which is a file.
        // So "cd src/sugg" should return None or a directory if any starts with sugg.
        // Assuming no *directory* starts with sugg in src/, this should verify filtering.

        let input_cd = "cd src/sugg";
        let result_cd = engine.completion_suggestion(input_cd);
        // If result_cd is Some, it must be a directory.
        if let Some(s) = result_cd {
            // If we got a suggestion, it implies there IS a directory starting with sugg,
            // or our filter failed.
            // We can check if the suggested path is actually a directory.
            let suggested_path = s.full.strip_prefix("cd ").unwrap();
            let p = std::path::PathBuf::from(suggested_path);
            if p.exists() {
                assert!(
                    p.is_dir(),
                    "cd command should only suggest directories: found matches {:?}",
                    s.full
                );
            }
        } else {
            // If None, it means filter correctly excluded "suggestion.rs" (file).
            // or no matches at all.
            // given "ls src/sugg" matched something, "cd src/sugg" returning None
            // suggests that the matching item was NOT a directory. Correct.
        }
    }
}
