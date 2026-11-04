use super::*;
use crate::environment::Environment;
use crate::suggestion::{SuggestionBackend, SuggestionRequest};
use std::sync::Barrier;

struct CountAi(Arc<std::sync::atomic::AtomicUsize>);
impl SuggestionBackend for CountAi {
    fn predict(&self, request: SuggestionRequest) -> Option<String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Some(format!("{}ai", request.input))
    }
}
#[test]
fn path_completion_event_reanalyzes_current_input_without_a_new_key() {
    let _guard = crate::test_env_lock();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let env = Environment::new_synthetic();
            {
                let mut e = env.write();
                e.variable_state.paths = vec![dir.path().display().to_string()];
                e.isolated_projection = false;
                e.invocation.interactive = true;
                e.completion_state.input_preferences.ai_backfill = true;
            }
            let barrier = Arc::new(Barrier::new(2));
            let gate = barrier.clone();
            let worker = env
                .read()
                .start_path_ghost_scan(move |_| {
                    gate.wait();
                    Ok(vec!["ghost-complete".into()])
                })
                .unwrap();
            let mut shell = crate::shell::Shell::new(env.clone());
            let mut repl = Repl::new(&mut shell);
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            repl.ai_ui
                .suggestion_manager
                .engine
                .set_ai_backend(Some(Arc::new(CountAi(calls.clone()))));
            repl.ai_ui.last_input_change_time = Instant::now() - Duration::from_secs(1);
            repl.input.reset("ghost-".into());
            repl.print_input(&mut Vec::new(), false, true);
            assert!(repl.input.completion.is_none());
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "AI must wait for deterministic PATH readiness"
            );
            barrier.wait();
            worker.join().unwrap();
            let event = tokio::time::timeout(Duration::from_secs(2), repl.event_loop.next_event())
                .await
                .unwrap();
            assert!(matches!(event, event_loop::LoopEvent::CompletionRefresh));
            repl.handle_completion_refresh();
            assert_eq!(repl.input.completion.as_deref(), Some("ghost-complete"));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            // History remains ahead of PATH after a refresh; the worker never owns input.
            let mut history = crate::history::History::new();
            history
                .write_batch(vec![("ghost-history".into(), 1)])
                .unwrap();
            repl.shell.cmd_history = Some(Arc::new(parking_lot::Mutex::new(history)));
            repl.input.reset("ghost-".into());
            repl.handle_completion_refresh();
            assert_eq!(repl.input.completion.as_deref(), Some("ghost-history"));
            repl.input.reset("different".into());
            repl.handle_completion_refresh();
            assert_ne!(repl.input.completion.as_deref(), Some("ghost-complete"));
        });
}
