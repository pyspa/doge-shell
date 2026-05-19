#[cfg(test)]
mod tests {
    use crate::environment::Environment;
    use crate::history::History;
    use crate::repl::Repl;
    use crate::repl::handler;
    use crate::repl::state::{InteractiveAction, ReplControlFlow};
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

        let down_result =
            handler::handle_key_event(&mut repl, &key(KeyCode::Down, KeyModifiers::NONE))
                .await
                .unwrap();
        assert!(matches!(down_result, ReplControlFlow::Continue));
        assert_eq!(repl.input.as_str(), "status");

        repl.input.reset("test".to_string());
        let next_up_result =
            handler::handle_key_event(&mut repl, &key(KeyCode::Up, KeyModifiers::NONE))
                .await
                .unwrap();
        assert!(matches!(next_up_result, ReplControlFlow::Continue));
        assert_eq!(repl.input.as_str(), "cargo test");
    }
}
