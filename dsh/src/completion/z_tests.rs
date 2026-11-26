#[cfg(test)]
mod tests {
    use crate::completion::selection::completion_for_z_with_path_history;
    use crate::history::FrecencyHistory;
    use crate::input::{Input, InputConfig};

    use skim::SkimItem;
    use std::sync::Arc;

    #[tokio::test]
    async fn z_history_requires_a_matching_query_in_the_active_invocation() {
        use crate::completion::framework::{CompletionFrameworkKind, CompletionSelection};
        let mut history = FrecencyHistory::new();
        history.add("work/my-project-list");
        let history = Arc::new(parking_lot::Mutex::new(history));
        for (line, query, expected) in [
            ("z project", "project", Some("work/my-project-list")),
            (
                "echo 界 && z\tproject",
                "project",
                Some("work/my-project-list"),
            ),
            ("z -l project", "project", Some("work/my-project-list")),
            ("z project§ --list", "project", Some("work/my-project-list")),
            (
                "z add nickname project",
                "project",
                Some("work/my-project-list"),
            ),
            ("z missing project", "project", None),
            ("z project§ missing", "project", None),
            ("z -", "-", None),
            ("z -l", "-l", None),
            ("z --li", "--li", None),
            ("z -proj", "-proj", Some("work/my-project-list")),
            ("z zzz-no-match", "zzz-no-match", None),
            ("z a; other project", "project", None),
            ("z > project", "project", None),
            ("z add project", "project", None),
            ("z rm project", "project", None),
        ] {
            let mut input = Input::new(InputConfig::default());
            input.reset(line.replace('§', ""));
            if let Some(offset) = line.find('§') {
                input.move_to(line[..offset].chars().count());
            }
            let actual = completion_for_z_with_path_history(
                &input,
                Some(&history),
                Some(query),
                "",
                line,
                CompletionFrameworkKind::Skim,
            );
            assert_eq!(
                actual,
                expected.map_or(
                    CompletionSelection::None,
                    |s| CompletionSelection::Selected(s.into())
                ),
                "{line}"
            );
        }
    }

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
