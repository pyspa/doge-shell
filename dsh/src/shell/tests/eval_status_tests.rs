use super::*;

fn test_shell() -> (Shell, Context) {
    let shell = Shell::new(crate::environment::Environment::new());
    *shell.environment.read().policy_state.safety_level.write() = crate::safety::SafetyLevel::Loose;
    for name in [
        "DOGESH_STATUS_MISSING",
        "RECOVERED",
        "CONTINUED",
        "SKIPPED",
        "SELECTED",
    ] {
        shell.environment.write().unset_shell_var(name);
    }
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, true);
    ctx.interactive = false;
    (shell, ctx)
}

#[tokio::test]
async fn ambiguous_redirect_fails_one_command_and_replaces_prior_status() {
    let _guard = SHELL_PROCESS_TEST_LOCK.lock().await;
    for prior in [0, 7] {
        let (mut shell, mut ctx) = test_shell();
        let seed = format!("sh -c 'exit {prior}'");
        assert_eq!(shell.eval_str(&mut ctx, seed, false).await.unwrap(), prior);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "").unwrap();
        std::fs::write(dir.path().join("b"), "").unwrap();
        // An ambiguous redirect is an ordinary command failure: it gates
        // `&&` and publishes status 1 instead of aborting the evaluation.
        let status = shell
            .eval_str(
                &mut ctx,
                format!("echo hidden > {}/* && RECOVERED=bad", dir.path().display()),
                false,
            )
            .await
            .unwrap();
        assert_eq!(status, 1, "prior status {prior}");
        assert_eq!(
            shell.environment.read().last_exit_status,
            1,
            "prior status {prior}"
        );
        // Expansion in a separate call must see the published failure.
        assert_eq!(
            shell
                .eval_str(&mut ctx, "OBSERVED=$?".into(), false)
                .await
                .unwrap(),
            0
        );
        assert_eq!(shell.environment.read().last_exit_status, 0);
        let env = shell.environment.read();
        assert_eq!(
            env.variable_state
                .variables
                .get("OBSERVED")
                .map(String::as_str),
            Some("1")
        );
        assert!(!env.variable_state.variables.contains_key("RECOVERED"));
    }
}

#[tokio::test]
async fn fatal_expansion_still_aborts_lists_and_success_preserves_gating() {
    let _guard = SHELL_PROCESS_TEST_LOCK.lock().await;
    for input in [
        "echo $((1/0)) || CONTINUED=bad",
        "echo ${DOGESH_STATUS_MISSING:?required} ; CONTINUED=bad",
        "echo '",
    ] {
        let (mut shell, mut ctx) = test_shell();
        assert!(shell.eval_str(&mut ctx, input.into(), false).await.is_err());
        assert_eq!(shell.environment.read().last_exit_status, 1);
        assert!(
            !shell
                .environment
                .read()
                .variable_state
                .variables
                .contains_key("CONTINUED")
        );
        assert_eq!(
            shell
                .eval_str(&mut ctx, "RECOVERY=ok".into(), false)
                .await
                .unwrap(),
            0
        );
        assert_eq!(shell.environment.read().last_exit_status, 0);
    }
    let (mut shell, mut ctx) = test_shell();
    assert_eq!(
        shell
            .eval_str(
                &mut ctx,
                "sh -c 'exit 7' && SKIPPED=bad || SELECTED=yes".into(),
                false
            )
            .await
            .unwrap(),
        0
    );
    assert!(
        !shell
            .environment
            .read()
            .variable_state
            .variables
            .contains_key("SKIPPED")
    );
    assert!(
        shell
            .environment
            .read()
            .variable_state
            .variables
            .contains_key("SELECTED")
    );
}
