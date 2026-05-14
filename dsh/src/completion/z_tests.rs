#[cfg(test)]
mod tests {
    use crate::completion::selection::completion_for_z_with_path_history;
    use crate::history::FrecencyHistory;
    use crate::input::{Input, InputConfig};

    use skim::SkimItem;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_completion_for_z_with_history() {
        // Setup History
        let mut history = FrecencyHistory::new();
        // Add some mock paths
        history.add("/tmp/mock_dir_a");
        history.add("/tmp/mock_dir_b");

        let path_history = Arc::new(parking_lot::Mutex::new(history));

        // Test Input "z "
        let mut input = Input::new(InputConfig::default());
        input.reset("z ".to_string());

        // Mock query/prompt/input_text
        let query = Some("");
        let prompt_text = "$ ";
        let input_text = "z ";

        // Execute completion
        let result = completion_for_z_with_path_history(
            &input,
            Some(&path_history),
            query,
            prompt_text,
            input_text,
            crate::completion::framework::CompletionFrameworkKind::Skim,
        );

        match result {
            crate::completion::framework::CompletionSelection::Interactive(candidates, query) => {
                assert_eq!(query.as_deref(), Some(""));
                assert!(
                    candidates
                        .iter()
                        .any(|candidate| { candidate.output().contains("/tmp/mock_dir_a") })
                );
                assert!(
                    candidates
                        .iter()
                        .any(|candidate| { candidate.output().contains("/tmp/mock_dir_b") })
                );
            }
            other => panic!("expected z completion candidates, got {other:?}"),
        }
    }
}
