//! Unconfigured AI actions must not collect context or block editing.
use super::*;
use crate::environment::Environment;
use crate::suggestion::{SuggestionBackend, SuggestionRequest};
use std::sync::atomic::AtomicUsize;

struct CountAi(Arc<AtomicUsize>);

impl SuggestionBackend for CountAi {
    fn predict(&self, _: SuggestionRequest) -> Option<String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        None
    }
}

async fn check_unconfigured_action(dispatch_key: bool) {
    let env = Environment::new_synthetic();
    assert!(!env.read().ai_configured());
    assert!(env.read().integration_state.ai_client.read().is_none());
    let mut shell = Shell::new(env);
    assert!(shell.cmd_history.is_none());
    let mut repl = Repl::new(&mut shell);
    assert!(repl.ai_service().is_none());
    let calls = Arc::new(AtomicUsize::new(0));
    repl.ai_ui
        .suggestion_manager
        .engine
        .set_ai_backend(Some(Arc::new(CountAi(calls.clone()))));
    let sentinel = Arc::new(vec!["synthetic-context".to_string()]);
    let path = std::path::PathBuf::from("synthetic-context-path");
    {
        let mut cache = repl.services.file_context.write();
        cache.path = path.clone();
        cache.files = sentinel.clone();
        cache.updated_at = None;
    }
    repl.input.reset("echo 日本語 draft".into());
    repl.input.move_word_left();
    let cursor = repl.input.cursor();
    let result = tokio::time::timeout(Duration::from_millis(300), async {
        if dispatch_key {
            let key = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('s'),
                crossterm::event::KeyModifiers::ALT,
            );
            assert!(matches!(
                repl.handle_key_event(&key).await.unwrap(),
                state::ReplControlFlow::Continue
            ));
        } else {
            assert!(!repl.force_ai_suggestion().await);
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "unconfigured Alt+s must return before 300ms"
    );
    assert_eq!(repl.input.as_str(), "echo 日本語 draft");
    assert_eq!(repl.input.cursor(), cursor);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let cache = repl.services.file_context.read();
    assert_eq!(cache.path, path);
    assert!(Arc::ptr_eq(&cache.files, &sentinel));
    assert!(cache.updated_at.is_none());
}

#[tokio::test]
async fn unconfigured_force_ai_returns_without_context_collection() {
    check_unconfigured_action(false).await;
}

#[tokio::test]
async fn unconfigured_alt_s_dispatch_preserves_input_and_cursor() {
    check_unconfigured_action(true).await;
}
