//! The AI-backed inline suggestion provider: the background worker that keeps
//! the completion request off the key-handling thread, its per-input and
//! per-directory caches, the two system prompts, and the response sanitising the
//! engine in `suggestion.rs` never sees.
//!
//! Flat sibling rather than `suggestion/ai_backend.rs` for the same reason as
//! `suggestion_tests.rs`: a `suggestion/` directory would make the `ls src/sugg`
//! prefix that `test_completion_suggestion_real_fs` relies on ambiguous.
use super::*;

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
    /// `AI_CHAT_MODEL`/`OPENAI_MODEL`, shared with the shell's `Environment`.
    /// Same slot `LiveAiService` reads, so `chat_model` reaches ghost text too
    /// instead of staying pinned to the model resolved when this backend was
    /// built.
    chat_model: Arc<RwLock<Option<String>>>,
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
    pub fn new(client: ChatGptClient, chat_model: Arc<RwLock<Option<String>>>) -> Self {
        Self::with_settings(client, chat_model, AiBackendSettings::default())
    }

    fn with_settings(
        client: ChatGptClient,
        chat_model: Arc<RwLock<Option<String>>>,
        settings: AiBackendSettings,
    ) -> Self {
        let inner = Arc::new(AiBackendInner {
            client: Arc::new(client),
            state: ParkingMutex::new(AiBackendState::default()),
            settings,
            notify: Notify::new(),
            chat_model,
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
        // No `max_completion_tokens`: on a reasoning model that budget also
        // covers hidden reasoning, and a capped ghost-text request comes back
        // empty with finish_reason=length.
        let options = ChatRequestOptions::new()
            .with_temperature(Some(self.inner.settings.temperature))
            .with_model(self.inner.chat_model.read().clone());
        let response = match self.inner.client.send_chat(&messages, &options, None) {
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

    // `SuggestionEngine::prefetch` (below) calls this through
    // `Arc<dyn SuggestionBackend>`, so this override - not an inherent method
    // of the same name - is what actually runs. An inherent `prefetch` here
    // used to shadow nothing: the trait object dispatch always found the
    // default no-op instead, so a cwd change never warmed the cache.
    fn prefetch(&self, request: SuggestionRequest) {
        if !request.input.is_empty() {
            return;
        }
        self.enqueue(request);
    }

    fn is_pending(&self) -> bool {
        let state = self.inner.state.lock();
        state.inflight || state.pending.is_some()
    }
}

pub(super) fn build_user_payload(request: &SuggestionRequest) -> String {
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

/// The ghost-text answer, or nothing.
///
/// Shared with the chat runtime so the array content shape and a reply the
/// provider cut short are handled the same way here as everywhere else; this
/// used to carry its own copy of the recursive text walker.
fn extract_ai_message_content(response: &Value) -> Option<String> {
    match dsh_openai::turn::answer_text(response) {
        Ok(content) => Some(content),
        Err(err) => {
            // A suggestion is speculative: report it in the log and show
            // nothing, rather than interrupting the prompt.
            debug!("ai suggestion produced no usable answer: {err}");
            None
        }
    }
}
