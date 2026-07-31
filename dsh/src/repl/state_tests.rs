#[cfg(test)]
mod tests {
    use crate::environment::Environment;
    use crate::history::History;
    use crate::input::ColorType;
    use crate::repl::Repl;
    use crate::repl::handler;
    use crate::repl::state::{InteractiveAction, ReplControlFlow, ShellEvent};
    use crate::shell::Shell;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use parking_lot::Mutex as ParkingMutex;
    use std::sync::Arc;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn test_interactive_action_creation() {
        let patch = InteractiveAction::Patch {
            text: "test".to_string(),
            backspace_count: 3,
        };
        match patch {
            InteractiveAction::Patch {
                text,
                backspace_count,
            } => {
                assert_eq!(text, "test");
                assert_eq!(backspace_count, 3);
            }
            _ => panic!("Expected Patch variant"),
        }

        let replace_range = InteractiveAction::ReplaceRange {
            start: 1,
            end: 4,
            text: "abc".to_string(),
        };
        match replace_range {
            InteractiveAction::ReplaceRange { start, end, text } => {
                assert_eq!(start, 1);
                assert_eq!(end, 4);
                assert_eq!(text, "abc");
            }
            _ => panic!("Expected ReplaceRange variant"),
        }

        let replace = InteractiveAction::ReplaceAll {
            text: "replacement".to_string(),
        };
        match replace {
            InteractiveAction::ReplaceAll { text } => {
                assert_eq!(text, "replacement");
            }
            _ => panic!("Expected ReplaceAll variant"),
        }
    }

    #[tokio::test]
    async fn enter_returns_execute_current_input() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.input.reset("echo hello".to_string());

        let result = handler::handle_key_event(&mut repl, &key(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(matches!(result, ReplControlFlow::ExecuteCurrentInput));
    }

    #[tokio::test]
    async fn alt_c_routes_smart_commit_through_execute_flow() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        let result =
            handler::handle_key_event(&mut repl, &key(KeyCode::Char('c'), KeyModifiers::ALT))
                .await
                .unwrap();

        assert!(matches!(result, ReplControlFlow::ExecuteCurrentInput));
        assert_eq!(repl.input.as_str(), "aic");
    }

    #[tokio::test]
    async fn ctrl_d_on_empty_input_sets_should_exit() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        let result =
            handler::handle_key_event(&mut repl, &key(KeyCode::Char('d'), KeyModifiers::CONTROL))
                .await
                .unwrap();

        assert!(matches!(result, ReplControlFlow::Continue));
        assert!(repl.state.should_exit);
    }

    #[tokio::test]
    async fn ctrl_d_mid_line_removes_char_under_cursor() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.input.reset("abc".to_string());
        repl.input.move_to_begin();

        handler::handle_key_event(&mut repl, &key(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        assert_eq!(repl.input.as_str(), "bc");
        assert!(!repl.state.should_exit);
    }

    #[tokio::test]
    async fn delete_key_removes_char_under_cursor() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.input.reset("あいう".to_string());
        repl.input.move_to_begin();

        handler::handle_key_event(&mut repl, &key(KeyCode::Delete, KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(repl.input.as_str(), "いう");
    }

    #[tokio::test]
    async fn delete_at_end_of_buffer_is_noop() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.input.reset("abc".to_string());

        handler::handle_key_event(&mut repl, &key(KeyCode::Delete, KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(repl.input.as_str(), "abc");
    }

    #[tokio::test]
    async fn ctrl_z_with_no_stopped_jobs_is_noop() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        let result =
            handler::handle_key_event(&mut repl, &key(KeyCode::Char('z'), KeyModifiers::CONTROL))
                .await
                .unwrap();

        assert!(matches!(result, ReplControlFlow::Continue));
        assert!(!repl.state.should_exit);
        assert_eq!(repl.input.as_str(), "");
    }

    #[tokio::test]
    async fn ctrl_o_with_no_blocks_returns_continue() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.terminal_ui.columns = 80;

        let result =
            handler::handle_key_event(&mut repl, &key(KeyCode::Char('o'), KeyModifiers::CONTROL))
                .await
                .unwrap();

        // Nothing recorded yet: say so instead of opening an empty full-screen UI.
        assert!(matches!(result, ReplControlFlow::Continue));
    }

    #[tokio::test]
    async fn ctrl_o_with_blocks_returns_run_interactive() {
        use dsh_types::command_block::CommandBlock;

        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        shell
            .environment
            .write()
            .session_output_state
            .command_blocks
            .push(CommandBlock::new(
                "cargo test".to_string(),
                Some("/repo".to_string()),
                0,
                120,
                &[],
                None,
            ));
        let mut repl = Repl::new(&mut shell);
        repl.input.reset("in progress".to_string());

        let result =
            handler::handle_key_event(&mut repl, &key(KeyCode::Char('o'), KeyModifiers::CONTROL))
                .await
                .unwrap();

        // Deliberately not invoking the closure: it would grab the tty.
        assert!(matches!(result, ReplControlFlow::RunInteractive(_)));
        assert_eq!(repl.input.as_str(), "in progress");
    }

    #[tokio::test]
    async fn replace_all_and_execute_variant_carries_the_command() {
        use crate::repl::state::InteractiveAction;

        let action = InteractiveAction::ReplaceAllAndExecute {
            text: "cd /repo".to_string(),
        };
        match action {
            InteractiveAction::ReplaceAllAndExecute { text } => assert_eq!(text, "cd /repo"),
            _ => panic!("expected ReplaceAllAndExecute"),
        }
    }

    #[tokio::test]
    async fn ctrl_r_returns_run_interactive() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut history = History::new();
        history
            .write_batch(vec![("cargo test".to_string(), 1)])
            .unwrap();
        shell.cmd_history = Some(Arc::new(ParkingMutex::new(history)));
        let mut repl = Repl::new(&mut shell);

        let result =
            handler::handle_key_event(&mut repl, &key(KeyCode::Char('r'), KeyModifiers::CONTROL))
                .await
                .unwrap();

        // Deliberately not invoking the closure: it would grab the tty.
        assert!(matches!(result, ReplControlFlow::RunInteractive(_)));
    }

    #[tokio::test]
    async fn ctrl_r_with_no_history_returns_continue() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        shell.cmd_history = Some(Arc::new(ParkingMutex::new(History::new())));
        let mut repl = Repl::new(&mut shell);

        let result =
            handler::handle_key_event(&mut repl, &key(KeyCode::Char('r'), KeyModifiers::CONTROL))
                .await
                .unwrap();

        // No entries to pick from: stay on the prompt rather than opening an
        // empty full-screen picker.
        assert!(matches!(result, ReplControlFlow::Continue));
    }

    #[tokio::test]
    async fn ctrl_r_preserves_the_input_until_the_picker_answers() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut history = History::new();
        history
            .write_batch(vec![("cargo test".to_string(), 1)])
            .unwrap();
        shell.cmd_history = Some(Arc::new(ParkingMutex::new(history)));
        let mut repl = Repl::new(&mut shell);
        repl.input.reset("car".to_string());

        handler::handle_key_event(&mut repl, &key(KeyCode::Char('r'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        assert_eq!(repl.input.as_str(), "car");
    }

    #[tokio::test]
    async fn check_background_jobs_with_no_jobs_is_silent() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.terminal_ui.columns = 80;
        repl.input.reset("in progress".to_string());

        crate::repl::key_handlers::auxiliary::check_background_jobs(&mut repl, true)
            .await
            .unwrap();

        // Nothing was reaped, so the in-progress input must be untouched.
        assert_eq!(repl.input.as_str(), "in progress");
    }

    #[tokio::test]
    async fn resize_event_updates_columns_and_lines() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.terminal_ui.columns = 80;
        repl.terminal_ui.lines = 24;
        repl.terminal_ui.last_drawn_cursor_y = 3;

        let result = handler::handle_event(
            &mut repl,
            ShellEvent::Input(crossterm::event::Event::Resize(120, 40)),
        )
        .await
        .unwrap();

        assert!(matches!(result, ReplControlFlow::Continue));
        assert_eq!(repl.terminal_ui.columns, 120);
        assert_eq!(repl.terminal_ui.lines, 40);
        // Stale geometry must be dropped: it was measured against the old width.
        // With an empty buffer the cursor is back on the mark row.
        assert_eq!(repl.terminal_ui.last_drawn_cursor_y, 0);
    }

    #[tokio::test]
    async fn resize_recomputes_cursor_row_for_the_new_width() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.terminal_ui.columns = 120;
        repl.terminal_ui.lines = 40;
        repl.terminal_ui.prompt_mark_cache = "> ".to_string();
        repl.terminal_ui.prompt_mark_width = 2;
        repl.input.reset("a".repeat(35));
        // Fits on one row at 120 columns.
        repl.terminal_ui.last_drawn_cursor_y = 0;

        handler::handle_event(
            &mut repl,
            ShellEvent::Input(crossterm::event::Event::Resize(16, 40)),
        )
        .await
        .unwrap();

        // "> " + 35 chars = 37 columns, which at 16 wide puts the cursor on
        // row 2. `print_input` needs that count to move back up to the mark.
        assert_eq!(repl.terminal_ui.columns, 16);
        assert_eq!(repl.terminal_ui.last_drawn_cursor_y, 2);
    }

    #[tokio::test]
    async fn resize_does_not_redraw_the_preprompt() {
        // Re-emitting it would leave the pre-resize copy on screen, stacking a
        // duplicate prompt on every resize.
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.terminal_ui.columns = 120;
        repl.terminal_ui.lines = 40;
        repl.terminal_ui.prompt_mark_cache = "> ".to_string();
        repl.terminal_ui.prompt_mark_width = 2;
        repl.terminal_ui.last_preprompt_plain = Some("~/repo".to_string());

        handler::handle_event(
            &mut repl,
            ShellEvent::Input(crossterm::event::Event::Resize(60, 20)),
        )
        .await
        .unwrap();

        // The recorded preprompt is untouched: nothing re-rendered it.
        assert_eq!(
            repl.terminal_ui.last_preprompt_plain.as_deref(),
            Some("~/repo")
        );
    }

    #[tokio::test]
    async fn resize_event_with_unchanged_size_keeps_geometry() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.terminal_ui.columns = 80;
        repl.terminal_ui.lines = 24;
        repl.terminal_ui.last_drawn_cursor_y = 3;

        handler::handle_event(
            &mut repl,
            ShellEvent::Input(crossterm::event::Event::Resize(80, 24)),
        )
        .await
        .unwrap();

        assert_eq!(repl.terminal_ui.columns, 80);
        assert_eq!(repl.terminal_ui.lines, 24);
        assert_eq!(repl.terminal_ui.last_drawn_cursor_y, 3);
    }

    #[tokio::test]
    async fn alt_x_routes_command_palette_through_outer_loop() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        let result =
            handler::handle_key_event(&mut repl, &key(KeyCode::Char('x'), KeyModifiers::ALT))
                .await
                .unwrap();

        assert!(matches!(result, ReplControlFlow::OpenCommandPalette));
    }

    #[tokio::test]
    async fn up_down_filter_history_by_substring_and_restore_input() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut history = History::new();
        history
            .write_batch(vec![
                ("git status".to_string(), 1),
                ("cargo test".to_string(), 2),
                ("docker status".to_string(), 3),
            ])
            .unwrap();
        shell.cmd_history = Some(Arc::new(ParkingMutex::new(history)));

        let mut repl = Repl::new(&mut shell);
        repl.input.reset("status".to_string());

        let up_result = handler::handle_key_event(&mut repl, &key(KeyCode::Up, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(matches!(up_result, ReplControlFlow::Continue));
        assert_eq!(repl.input.as_str(), "docker status");
        assert_eq!(
            repl.input.color_ranges.as_deref(),
            Some(
                &[
                    (0, 6, ColorType::CommandExists),
                    (7, 13, ColorType::HistoryMatch)
                ][..]
            )
        );

        let down_result =
            handler::handle_key_event(&mut repl, &key(KeyCode::Down, KeyModifiers::NONE))
                .await
                .unwrap();
        assert!(matches!(down_result, ReplControlFlow::Continue));
        assert_eq!(repl.input.as_str(), "status");
        assert!(
            !repl
                .input
                .color_ranges
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .any(|(_, _, kind)| matches!(kind, ColorType::HistoryMatch))
        );

        repl.input.reset("test".to_string());
        let next_up_result =
            handler::handle_key_event(&mut repl, &key(KeyCode::Up, KeyModifiers::NONE))
                .await
                .unwrap();
        assert!(matches!(next_up_result, ReplControlFlow::Continue));
        assert_eq!(repl.input.as_str(), "cargo test");
        assert_eq!(
            repl.input.color_ranges.as_deref(),
            Some(
                &[
                    (0, 5, ColorType::CommandExists),
                    (6, 10, ColorType::HistoryMatch)
                ][..]
            )
        );
    }
}
