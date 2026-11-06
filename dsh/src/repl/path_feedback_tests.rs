use super::*;
use crate::environment::Environment;
use std::sync::mpsc;

#[test]
fn path_feedback_tab_pending_then_ready_including_empty() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "path_feedback_tab_pending_then_ready_including_empty",
    ) {
        return;
    }
    let _guard = crate::test_env_lock();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for result in [
                Ok(vec!["zz-feedback-ready".into()]),
                Ok(vec![]),
                Err(anyhow::anyhow!("fixture failure")),
            ] {
                let succeeds = result.is_ok();
                let dir = tempfile::tempdir().unwrap();
                let env = Environment::new_synthetic();
                {
                    let mut e = env.write();
                    e.variable_state.paths = vec![dir.path().display().to_string()];
                    e.isolated_projection = false;
                    e.invocation.interactive = true;
                }
                let (started_tx, started_rx) = mpsc::channel();
                let (release_tx, release_rx) = mpsc::channel();
                let worker = env
                    .read()
                    .start_path_ghost_scan(move |_| {
                        started_tx.send(()).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(5))?;
                        result
                    })
                    .unwrap();
                started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                let mut shell = Shell::new(env);
                let mut repl = Repl::new(&mut shell);
                repl.terminal_ui.columns = 120;
                repl.input.reset("zz-feedback-no-match".into());
                key_handlers::completion::handle_trigger_completion(&mut repl)
                    .await
                    .unwrap();
                let mut out = Vec::new();
                repl.print_input(&mut out, false, false);
                assert!(
                    String::from_utf8_lossy(&out).contains("PATH scanning…"),
                    "{}",
                    String::from_utf8_lossy(&out)
                );
                // An unrelated provider notification must keep the scan pending.
                repl.handle_completion_refresh();
                out.clear();
                repl.print_input(&mut out, false, false);
                assert!(String::from_utf8_lossy(&out).contains("PATH scanning…"));
                release_tx.send(()).unwrap();
                worker.join().unwrap();
                repl.handle_completion_refresh();
                out.clear();
                repl.print_input(&mut out, false, false);
                assert_eq!(
                    String::from_utf8_lossy(&out).contains("PATH ready · Tab again"),
                    succeeds
                );
                assert!(!String::from_utf8_lossy(&out).contains("PATH scanning…"));
                assert_eq!(repl.input.as_str(), "zz-feedback-no-match");
                repl.terminal_ui.columns = 15;
                out.clear();
                repl.print_input(&mut out, false, false);
                assert!(!String::from_utf8_lossy(&out).contains("PATH ready"));
                repl.terminal_ui.columns = 120;
                handler::handle_key_event(
                    &mut repl,
                    &crossterm::event::KeyEvent::new(
                        crossterm::event::KeyCode::Esc,
                        crossterm::event::KeyModifiers::NONE,
                    ),
                )
                .await
                .unwrap();
                out.clear();
                repl.print_input(&mut out, false, false);
                assert!(!String::from_utf8_lossy(&out).contains("PATH ready"));
            }
        });
}

#[test]
fn path_feedback_receipt_is_consumed_before_file_only_tab() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "path_feedback_receipt_is_consumed_before_file_only_tab",
    ) {
        return;
    }
    let _guard = crate::test_env_lock();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut shell = Shell::new(Environment::new_synthetic());
            let mut repl = Repl::new(&mut shell);
            let receipt = crate::completion::generators::system::SystemCommandScanStatus {
                generation: 3,
                latest_scan: 7,
                pending: Some(7),
                published: 0,
                complete: false,
            };
            let runtime = repl.completion_ui.integrated_completion.runtime();
            runtime.record_path_request(receipt);
            repl.input.reset("cat /zz-feedback-absent-dir/file".into());
            key_handlers::completion::handle_trigger_completion(&mut repl)
                .await
                .unwrap();
            assert!(runtime.take_path_request().is_none());
            assert!(!repl.completion_ui.path_feedback.is_active());
            // Even cached input analysis is never a user request for feedback.
            repl.print_input(&mut Vec::new(), false, false);
            assert!(!repl.completion_ui.path_feedback.is_active());
        });
}

#[test]
fn path_feedback_render_reserves_history_ghost_and_wrapped_rows() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "path_feedback_render_reserves_history_ghost_and_wrapped_rows",
    ) {
        return;
    }
    let _guard = crate::test_env_lock();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let dir = tempfile::tempdir().unwrap();
    let env = Environment::new_synthetic();
    env.write().variable_state.paths = vec![dir.path().display().to_string()];
    let mut shell = Shell::new(env);
    let mut repl = Repl::new(&mut shell);
    assert!(!repl.terminal_ui.status_line.borrow().is_enabled());
    let mut history = crate::history::History::new();
    history
        .write_batch(vec![(format!("zz{}", "x".repeat(90)), 1)])
        .unwrap();
    repl.shell.cmd_history = Some(Arc::new(parking_lot::Mutex::new(history)));
    let system = crate::completion::generator::activate_system_command_cache(&[dir
        .path()
        .display()
        .to_string()]);
    let scan = crate::completion::generator::begin_background_system_command_scan(&system);
    let receipt = crate::completion::generators::system::system_command_scan_status();
    repl.input.reset("zz".into());
    repl.completion_ui
        .path_feedback
        .request("zz", 2, 0, receipt);
    repl.terminal_ui.columns = 100;
    let mut out = Vec::new();
    repl.print_input(&mut out, false, false);
    assert!(
        !String::from_utf8_lossy(&out).contains("PATH scanning"),
        "long ghost reserves cells"
    );
    repl.terminal_ui.columns = 140;
    out.clear();
    repl.print_input(&mut out, false, false);
    assert!(String::from_utf8_lossy(&out).contains("PATH scanning"));
    repl.terminal_ui.columns = 1;
    out.clear();
    repl.print_input(&mut out, false, false);
    assert!(!String::from_utf8_lossy(&out).contains("PATH scanning"));
    crate::completion::generator::release_system_command_scan(&scan);
    repl.terminal_ui.columns = 140;
    out.clear();
    repl.print_input(&mut out, false, false);
    assert!(
        !String::from_utf8_lossy(&out).contains("PATH scanning"),
        "release clears feedback without provider readiness"
    );
}

#[test]
fn path_feedback_user_actions_clear_before_dispatch_and_explicit_accept_cancel() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "path_feedback_user_actions_clear_before_dispatch_and_explicit_accept_cancel",
    ) {
        return;
    }
    let _guard = crate::test_env_lock();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut shell = Shell::new(Environment::new_synthetic());
            let mut repl = Repl::new(&mut shell);
            let receipt = crate::completion::generators::system::SystemCommandScanStatus {
                generation: 3,
                latest_scan: 7,
                pending: Some(7),
                published: 0,
                complete: false,
            };
            for key in [
                crossterm::event::KeyCode::Char('x'),
                crossterm::event::KeyCode::Left,
                crossterm::event::KeyCode::Esc,
            ] {
                repl.input.reset("zz".into());
                repl.completion_ui
                    .path_feedback
                    .request("zz", 2, 0, receipt);
                handler::handle_key_event(
                    &mut repl,
                    &crossterm::event::KeyEvent::new(key, crossterm::event::KeyModifiers::NONE),
                )
                .await
                .unwrap();
                assert!(!repl.completion_ui.path_feedback.is_active());
            }
            repl.completion_ui
                .path_feedback
                .request("zz", 2, 0, receipt);
            key_handlers::editing::handle_paste_event(&mut repl, "x")
                .await
                .unwrap();
            assert!(!repl.completion_ui.path_feedback.is_active());
            repl.completion_ui
                .path_feedback
                .request("zz", 2, 0, receipt);
            key_handlers::completion::handle_cancel_completion(&mut repl);
            assert!(!repl.completion_ui.path_feedback.is_active());
            repl.input.reset("zz".into());
            repl.input.completion = Some("zz-accepted".into());
            repl.completion_ui
                .path_feedback
                .request("zz", 2, 0, receipt);
            key_handlers::completion::handle_accept_completion(&mut repl);
            assert!(!repl.completion_ui.path_feedback.is_active());
            assert_eq!(repl.input.as_str(), "zz-accepted");
        });
}

#[test]
fn path_feedback_pending_backend_owns_row_until_settled() {
    if crate::completion::isolation_tests::run_isolated(
        module_path!(),
        "path_feedback_pending_backend_owns_row_until_settled",
    ) {
        return;
    }
    struct PendingBackend(Arc<AtomicBool>);
    impl crate::suggestion::SuggestionBackend for PendingBackend {
        fn predict(&self, _: crate::suggestion::SuggestionRequest) -> Option<String> {
            None
        }
        fn is_pending(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }
    let _guard = crate::test_env_lock();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let dir = tempfile::tempdir().unwrap();
    let env = Environment::new_synthetic();
    env.write().variable_state.paths = vec![dir.path().display().to_string()];
    let mut shell = Shell::new(env);
    let mut repl = Repl::new(&mut shell);
    let pending = Arc::new(AtomicBool::new(true));
    repl.ai_ui
        .suggestion_manager
        .set_preferences(crate::suggestion::InputPreferences {
            ai_backfill: true,
            ..Default::default()
        });
    repl.ai_ui
        .suggestion_manager
        .engine
        .set_ai_backend(Some(Arc::new(PendingBackend(pending.clone()))));
    let activation = crate::completion::generator::activate_system_command_cache(&[dir
        .path()
        .display()
        .to_string()]);
    let scan = crate::completion::generator::begin_background_system_command_scan(&activation);
    let receipt = crate::completion::generators::system::system_command_scan_status();
    repl.input.reset("zz-unmatched".into());
    repl.completion_ui
        .path_feedback
        .request("zz-unmatched", 12, 0, receipt);
    repl.terminal_ui.columns = 120;
    let mut out = Vec::new();
    repl.print_input(&mut out, false, false);
    assert!(String::from_utf8_lossy(&out).contains(" ⧗"));
    assert!(
        !String::from_utf8_lossy(&out).contains("PATH scanning"),
        "pending marker must not follow a hint ending in the final column"
    );
    pending.store(false, Ordering::SeqCst);
    out.clear();
    repl.print_input(&mut out, false, false);
    assert!(
        String::from_utf8_lossy(&out).contains("PATH scanning"),
        "coexistence suppression must retain the request"
    );
    assert!(crate::completion::generator::publish_system_command_scan(
        &scan,
        Default::default()
    ));
    pending.store(true, Ordering::SeqCst);
    out.clear();
    repl.print_input(&mut out, false, false);
    assert!(String::from_utf8_lossy(&out).contains(" ⧗"));
    assert!(!String::from_utf8_lossy(&out).contains("PATH ready"));
    pending.store(false, Ordering::SeqCst);
    out.clear();
    repl.print_input(&mut out, false, false);
    assert!(String::from_utf8_lossy(&out).contains("PATH ready · Tab again"));
}
