#[cfg(test)]
mod tests {
    use crate::repl::state::InteractiveAction;

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
}
