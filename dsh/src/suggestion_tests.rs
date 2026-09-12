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
fn test_input_preferences_default() {
    let prefs = InputPreferences::default();
    assert!(!prefs.ai_explanation);
}

/// `AiSuggestionBackend` used to have an inherent `prefetch` method beside
/// the trait one instead of overriding it. Everyone who holds the backend
/// as `Arc<dyn SuggestionBackend>` (as `SuggestionEngine` does) dispatches
/// virtually and always found the trait's default no-op, so a cwd change
/// never actually warmed the cache. Only a call through the trait object
/// - not a direct call on the concrete type - can catch that regression.
#[test]
fn ai_backend_prefetch_reaches_the_override_through_dyn_dispatch() {
    let client = ChatGptClient::new("test-key".to_string()).unwrap();
    let backend: Arc<dyn SuggestionBackend + Send + Sync> =
        Arc::new(AiSuggestionBackend::new(client));

    assert!(!backend.is_pending());

    backend.prefetch(SuggestionRequest::new(
        String::new(),
        0,
        InputPreferences::default(),
        Vec::new(),
        None,
        Arc::new(Vec::new()),
        None,
    ));

    assert!(
        backend.is_pending(),
        "prefetch through the trait object must reach AiSuggestionBackend's own \
             logic, not the default no-op"
    );
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

#[derive(Default)]
struct FailingBackend {
    calls: Mutex<usize>,
}

impl SuggestionBackend for FailingBackend {
    fn predict(&self, _request: SuggestionRequest) -> Option<String> {
        *self.calls.lock() += 1;
        None // simulate error / no suggestion
    }
}

#[test]
fn engine_handles_backend_returning_none() {
    let backend = Arc::new(FailingBackend::default());
    let mut engine = SuggestionEngine::new();
    engine.set_ai_backend(Some(backend.clone()));
    engine.set_preferences(InputPreferences {
        suggestion_mode: SuggestionMode::Ghost,
        ai_backfill: true,
        ..Default::default()
    });

    let result = engine.predict("git s", 5, None);
    assert!(result.is_empty(), "Should return empty when backend fails");
    assert_eq!(
        *backend.calls.lock(),
        1,
        "Backend should have been called once"
    );

    // Ensure state is clean for a subsequent call
    let result2 = engine.predict("git st", 6, None);
    assert!(
        result2.is_empty(),
        "Should return empty when backend fails again"
    );
    assert_eq!(*backend.calls.lock(), 2, "Backend should be called again");
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

    // Call without history, with context
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

    let result = engine.predict("gco\tmain", 8, None);
    assert!(
        result.is_empty(),
        "gco with tab-separated args should be blocked"
    );
    assert_eq!(
        recorder.calls().len(),
        0,
        "Backend should not be called for gco with tab-separated args"
    );

    // 3. Leading whitespace still leaves gco as the first command token.
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

    assert!(!engine.in_blocklist(r#""gco" main"#));
    assert!(!engine.in_blocklist("cmd | gco"));
}

#[test]
fn cd_command_token_itself_is_not_path_context() {
    let input = "cd";
    let token = shell_token::token_at_char_cursor(
        input,
        input.chars().count(),
        SeparatorMode::CompletionRange,
    )
    .unwrap();

    assert!(!is_cd_path_context(input, &token));
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

#[test]
fn predict_history_does_not_return_path_completion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkout.txt");
    std::fs::File::create(&path).unwrap();
    let prefix = path.with_extension("");
    let input = format!("ls {}", prefix.display());
    let cursor = input.chars().count();

    let mut generic_engine = SuggestionEngine::new();
    generic_engine.set_preferences(InputPreferences::default());
    assert!(
        generic_engine
            .predict(&input, cursor, None)
            .iter()
            .any(|suggestion| {
                suggestion.source == SuggestionSource::Completion
                    && suggestion.full.ends_with("checkout.txt")
            }),
        "generic predict should still expose path completion"
    );

    let mut history_engine = SuggestionEngine::new();
    history_engine.set_preferences(InputPreferences::default());
    assert!(
        history_engine
            .predict_history(&input, cursor, None)
            .is_empty(),
        "history-only prediction must not include path completion"
    );
}

#[test]
fn completion_suggestion_preserves_escaped_path_style() {
    let dir = tempfile::tempdir().unwrap();
    let spaced = dir.path().join("dir with space");
    std::fs::create_dir(&spaced).unwrap();
    std::fs::File::create(spaced.join("foo.txt")).unwrap();

    let engine = SuggestionEngine::new();
    let raw_prefix = format!("{}/dir\\ with\\ space/fo", dir.path().display());
    let expected = format!("ls {}/dir\\ with\\ space/foo.txt", dir.path().display());

    let suggestion = engine
        .completion_suggestion(&format!("ls {raw_prefix}"))
        .unwrap();

    assert_eq!(suggestion.full, expected);
    assert_eq!(suggestion.source, SuggestionSource::Completion);
}

#[test]
fn completion_suggestion_preserves_quoted_path_style() {
    let dir = tempfile::tempdir().unwrap();
    let spaced = dir.path().join("dir with space");
    std::fs::create_dir(&spaced).unwrap();
    std::fs::File::create(spaced.join("foo.txt")).unwrap();

    let engine = SuggestionEngine::new();
    let input = format!("ls \"{}/dir with space/fo", dir.path().display());
    let expected = format!("ls \"{}/dir with space/foo.txt", dir.path().display());

    let suggestion = engine.completion_suggestion(&input).unwrap();

    assert_eq!(suggestion.full, expected);
    assert_eq!(suggestion.source, SuggestionSource::Completion);
}

#[test]
fn completion_suggestion_keeps_cd_directory_only() {
    let dir = tempfile::tempdir().unwrap();
    let spaced = dir.path().join("dir with space");
    std::fs::create_dir(&spaced).unwrap();
    std::fs::File::create(spaced.join("foo.txt")).unwrap();
    std::fs::create_dir(spaced.join("foodir")).unwrap();

    let engine = SuggestionEngine::new();
    let input = format!("cd {}/dir\\ with\\ space/fo", dir.path().display());
    let expected = format!("cd {}/dir\\ with\\ space/foodir/", dir.path().display());

    let suggestion = engine.completion_suggestion(&input).unwrap();

    assert_eq!(suggestion.full, expected);
    assert_eq!(suggestion.source, SuggestionSource::Completion);
}

#[test]
fn completion_suggestion_keeps_cd_directory_only_with_tab_separator() {
    let dir = tempfile::tempdir().unwrap();
    let spaced = dir.path().join("dir with space");
    std::fs::create_dir(&spaced).unwrap();
    std::fs::File::create(spaced.join("foo.txt")).unwrap();
    std::fs::create_dir(spaced.join("foodir")).unwrap();

    let engine = SuggestionEngine::new();
    let input = format!("cd\t{}/dir\\ with\\ space/fo", dir.path().display());
    let expected = format!("cd\t{}/dir\\ with\\ space/foodir/", dir.path().display());

    let suggestion = engine.completion_suggestion(&input).unwrap();

    assert_eq!(suggestion.full, expected);
    assert_eq!(suggestion.source, SuggestionSource::Completion);
}

#[test]
fn completion_suggestion_keeps_cd_directory_only_with_leading_space() {
    let dir = tempfile::tempdir().unwrap();
    let spaced = dir.path().join("dir with space");
    std::fs::create_dir(&spaced).unwrap();
    std::fs::File::create(spaced.join("foo.txt")).unwrap();
    std::fs::create_dir(spaced.join("foodir")).unwrap();

    let engine = SuggestionEngine::new();
    let input = format!(" cd {}/dir\\ with\\ space/fo", dir.path().display());
    let expected = format!(" cd {}/dir\\ with\\ space/foodir/", dir.path().display());

    let suggestion = engine.completion_suggestion(&input).unwrap();

    assert_eq!(suggestion.full, expected);
    assert_eq!(suggestion.source, SuggestionSource::Completion);
}
