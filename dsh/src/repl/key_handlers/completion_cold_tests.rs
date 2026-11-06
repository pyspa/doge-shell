use super::*;
use crate::{environment::Environment, shell::Shell};
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn first_tab_waits_for_systemctl_units_instead_of_inserting_directory() {
    for input in ["systemctl restart ", "sudo systemctl restart "] {
        let dir = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("wrong-directory")).unwrap();
        let command = bin.path().join("systemctl");
        std::fs::write(
            &command,
            "#!/bin/sh\nsleep 0.1\nprintf 'cold-test.service loaded active running Test\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755)).unwrap();
        let environment = Environment::new_synthetic();
        {
            let mut env = environment.write();
            env.variable_state.paths = vec![bin.path().display().to_string()];
            env.set_shell_var("DOGESH_COMPLETION_FISH_FALLBACK".into(), "off".into());
        }
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);
        repl.completion_ui
            .integrated_completion
            .initialize_command_completion()
            .unwrap();
        repl.terminal_ui.prompt.write().set_current(dir.path());
        repl.input.reset(input.into());
        handle_trigger_completion(&mut repl).await.unwrap();
        assert_eq!(repl.input.as_str(), format!("{input}cold-test.service"));
    }
}

#[tokio::test]
async fn unrelated_refresh_does_not_delay_synchronous_dynamic_completion() {
    let bin = tempfile::tempdir().unwrap();
    let command = bin.path().join("systemctl");
    std::fs::write(
        &command,
        "#!/bin/sh\nsleep 0.5\nprintf 'cold-test.service loaded active running Test\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = Environment::new_synthetic();
    env.write().variable_state.paths = vec![bin.path().display().to_string()];
    let mut engine = crate::completion::integrated::IntegratedCompletionEngine::new(env);
    engine.initialize_command_completion().unwrap();
    let mut refresh = engine.runtime().subscribe_refresh();
    let input = "systemctl restart ";
    let first = engine
        .complete(input, input.len(), bin.path(), 50, None)
        .await;
    assert_eq!(first.disposition, CompletionDisposition::Pending);
    let other = engine.complete("fg ", 3, bin.path(), 50, None).await;
    tokio::time::timeout(std::time::Duration::from_secs(2), refresh.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other.disposition, CompletionDisposition::Allowed);
}
