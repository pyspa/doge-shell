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

#[tokio::test]
async fn only_commands_reach_aggregate_history_and_the_output_ledger() {
    let _guard = SHELL_PROCESS_TEST_LOCK.lock().await;
    let (mut shell, mut ctx) = test_shell();
    ctx.save_history = true;
    let root = tempfile::tempdir().unwrap();
    let db = crate::db::Db::new(root.path().join("history.db")).unwrap();
    let mut history = crate::history::History::new();
    history.db = Some(db.clone());
    shell.cmd_history = Some(Arc::new(ParkingMutex::new(history)));
    shell.environment.write().variable_state.command_ledger_mode =
        crate::history::CommandLedgerMode::Output;

    for input in [
        "!&invalid",
        "blocks explain 1;",
        "\"blocks\" explain 1",
        "CHECK_HISTORY='! literal'",
    ] {
        let code = shell
            .eval_str(&mut ctx, input.into(), false)
            .await
            .unwrap_or(1);
        shell.record_history_outcome(
            input,
            code,
            std::time::Duration::ZERO,
            Some("synthetic output"),
        );
    }
    shell.record_pipe_command("! false", 0, std::time::Duration::ZERO, "child output");
    // Inspect both raw tables so hiding a bad row at read time cannot pass this test.
    for table in ["command_history", "command_events"] {
        let connection = db.get_connection();
        let mut statement = connection
            .prepare(&format!("SELECT command FROM {table} ORDER BY id"))
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rows, ["CHECK_HISTORY='! literal'", "! false"], "{table}");
    }
    let output: String = db
        .get_connection()
        .query_row(
            "SELECT output FROM command_events WHERE command = '! false'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(output, "child output");
}
