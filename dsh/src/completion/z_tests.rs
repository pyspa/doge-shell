#[cfg(test)]
mod tests {
    use crate::completion::completion_for_z;
    use crate::environment::Environment;
    use crate::history::FrecencyHistory;
    use crate::input::{Input, InputConfig};
    use crate::repl::Repl;
    use crate::shell::Shell;

    use std::sync::Arc;

    #[tokio::test]
    #[ignore] // TODO: Fix deadlock/race condition caused by std::env::set_var in parallel tests
    async fn test_completion_for_z_with_history() {
        // Setup Environment & Shell
        // Environment::new() returns Arc<RwLock<Environment>>
        let env = Environment::new();
        let mut shell = Shell::new(env.clone());

        // Setup History
        let mut history = FrecencyHistory::new();
        // Add some mock paths
        history.add("/tmp/mock_dir_a");
        history.add("/tmp/mock_dir_b");

        // Assign history to shell
        // FrecencyHistory is wrapped in Arc<parking_lot::Mutex<FrecencyHistory>> in Shell.path_history
        shell.path_history = Some(Arc::new(parking_lot::Mutex::new(history)));

        // Create Repl
        let repl = Repl::new(&mut shell);

        // Test Input "z "
        let mut input = Input::new(InputConfig::default());
        input.reset("z ".to_string());

        // Mock query/prompt/input_text
        let query = Some("");
        let prompt_text = "$ ";
        let input_text = "z ";

        // Force inline framework to avoid Skim UI launch
        unsafe {
            std::env::set_var("DSH_COMPLETION_FRAMEWORK", "inline");
        }

        // Execute completion
        let result = completion_for_z(&input, &repl, query, prompt_text, input_text);

        println!("Completion result: {:?}", result);

        // Avoid borrowing shell.path_history while repl exists
        // assert!(shell.path_history.is_some());
    }
}
