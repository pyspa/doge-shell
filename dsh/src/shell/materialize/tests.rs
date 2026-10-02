use super::super::authorize::is_authorization_cancelled;
use super::super::dry_materialize::dry_materialize_job;
use super::*;
use crate::repl::confirmation::ConfirmationAction;
use std::sync::Arc;

fn deny_all(_: &str) -> Result<ConfirmationAction> {
    Ok(ConfirmationAction::No)
}

/// Test H: nested `echo $(echo $(rm ...))` denies from the inside out.
/// Nothing runs and the victim directory survives.
#[tokio::test]
async fn nested_denial_aborts_the_whole_chain() {
    let dir = tempfile::tempdir().expect("tempdir");
    let victim = dir.path().join("victim");
    std::fs::create_dir(&victim).expect("victim");
    let input = format!("echo $(echo $(rm -rf {}))", victim.display());
    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env);
    let plan = super::super::parse::parse_execution_plan(&input, Arc::clone(&shell.environment))
        .expect("plan");
    assert_eq!(plan.lists.len(), 1);
    assert!(plan.lists[0].jobs[0].contains_dynamic_expansion());
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], deny_all).await {
        Err(err) => assert!(
            is_authorization_cancelled(&err),
            "nested denial must surface as cancellation, got {err:?}"
        ),
        Ok(_) => panic!("nested dangerous body must not materialize"),
    }
    assert!(victim.exists(), "denied nested body must not run");
}

/// Unselected `${X:-$(rm ...)}` never authorizes or runs the operand.
#[tokio::test]
async fn unselected_parameter_operand_needs_no_authorization() {
    fn allow_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::Yes)
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let victim = dir.path().join("victim");
    std::fs::create_dir(&victim).expect("victim");
    let input = format!(
        "echo ${{DOGESH_SAFE_PARAM:-$(rm -rf {})}}",
        victim.display()
    );
    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env);
    shell
        .environment
        .write()
        .set_shell_var("DOGESH_SAFE_PARAM".to_string(), "safe".to_string());
    let plan = super::super::parse::parse_execution_plan(&input, Arc::clone(&shell.environment))
        .expect("plan");
    // Even with `deny_all`, the unselected operand produces no prompt
    // and no process: materialization succeeds with the parameter value.
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], deny_all)
        .await
        .expect("unselected operand must materialize")
    {
        MaterializeOutcome::Runnable(materialized) => {
            let argv = materialized
                .job
                .process
                .as_ref()
                .expect("process")
                .command_argv()
                .expect("argv");
            assert_eq!(argv.1.to_vec(), vec!["safe".to_string()]);
        }
        MaterializeOutcome::NoCommand(_) => panic!("expected runnable"),
        MaterializeOutcome::Rejected(f) => panic!("unexpected rejection: {f:?}"),
    }
    assert!(victim.exists(), "unselected rm must not run");
    let _ = allow_all;
}

/// Selected `${UNSET:-$(rm ...)}` goes through the existing
/// authorize-before-run path and is denyable.
#[tokio::test]
async fn selected_parameter_operand_is_denyable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let victim = dir.path().join("victim");
    std::fs::create_dir(&victim).expect("victim");
    let input = format!(
        "echo ${{DOGESH_UNSET_PARAM:-$(rm -rf {})}}",
        victim.display()
    );
    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env);
    let plan = super::super::parse::parse_execution_plan(&input, Arc::clone(&shell.environment))
        .expect("plan");
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], deny_all).await {
        Err(err) => assert!(
            is_authorization_cancelled(&err),
            "selected denial must surface as cancellation, got {err:?}"
        ),
        Ok(_) => panic!("denied selected operand must not materialize"),
    }
    assert!(victim.exists(), "denied selected operand must not run");
}

/// A variable-derived command is dynamic: the concrete argv is `rm ...`
/// and the raw source must not bypass the guard.
#[tokio::test]
async fn dynamic_command_reports_dynamic_expansion() {
    use crate::repl::confirmation::ConfirmationAction;

    fn allow_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::Yes)
    }

    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env);
    shell
        .environment
        .write()
        .set_shell_var("CMD".to_string(), "rm".to_string());
    let plan = super::super::parse::parse_execution_plan(
        "$CMD -rf victim",
        Arc::clone(&shell.environment),
    )
    .expect("plan");
    assert!(plan.lists[0].jobs[0].contains_dynamic_expansion());
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    let materialized = match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], allow_all)
        .await
        .expect("materialize")
    {
        MaterializeOutcome::Runnable(materialized) => materialized,
        MaterializeOutcome::NoCommand(_) => panic!("expected runnable job"),
        MaterializeOutcome::Rejected(failure) => {
            panic!("expected runnable job, got rejection: {failure:?}")
        }
    };
    assert!(materialized.had_dynamic_expansion);
    let argv = materialized
        .job
        .process
        .as_ref()
        .expect("process")
        .command_argv()
        .expect("concrete argv");
    assert_eq!(argv.0, "rm");
    assert_eq!(
        argv.1.to_vec(),
        vec!["-rf".to_string(), "victim".to_string()]
    );
}

/// A `NAME=value` prefix on a builtin is an expected command failure, not
/// a silently dropped stage: the outcome is `Rejected` with a non-zero
/// status and a diagnostic, never `Runnable` and never `NoCommand`.
#[tokio::test]
async fn builtin_prefix_is_rejected_as_command_failure() {
    fn allow_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::Yes)
    }

    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    let plan =
        super::super::parse::parse_execution_plan("FOO=bar alias", Arc::clone(&env)).expect("plan");
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], allow_all)
        .await
        .expect("materialize")
    {
        MaterializeOutcome::Rejected(failure) => {
            assert_eq!(failure.exit_code, BUILTIN_ENV_PREFIX_EXIT_CODE);
            assert_ne!(failure.exit_code, 0);
            assert!(failure.message.contains("not supported for builtins"));
            assert!(failure.message.contains("alias"));
        }
        MaterializeOutcome::Runnable(_) => panic!("builtin prefix must not be runnable"),
        MaterializeOutcome::NoCommand(_) => panic!("builtin prefix is not no-command"),
    }
}

/// An exported Lisp command uses the same builtin path, so it shares the
/// same refusal semantics.
#[tokio::test]
async fn exported_lisp_prefix_is_rejected_like_builtin() {
    fn allow_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::Yes)
    }

    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    // Only `fn` marks the lambda as exported (`LispEngine::is_export`);
    // `defun` would never be callable as a shell command.
    shell
        .lisp_engine
        .borrow()
        .run("(fn exported-prefix-probe () 1)")
        .expect("fn");
    let plan = super::super::parse::parse_execution_plan(
        "FOO=bar exported-prefix-probe",
        Arc::clone(&env),
    )
    .expect("plan");
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], allow_all)
        .await
        .expect("materialize")
    {
        MaterializeOutcome::Rejected(failure) => {
            assert_eq!(failure.exit_code, BUILTIN_ENV_PREFIX_EXIT_CODE);
        }
        MaterializeOutcome::Runnable(_) => panic!("expected rejection, got runnable"),
        MaterializeOutcome::NoCommand(_) => panic!("expected rejection, got no-command"),
    }
}

/// An external command with a prefix stays runnable and keeps the
/// overrides on the process, not in the shell.
#[tokio::test]
async fn external_prefix_stays_runnable() {
    // Materialization never spawns, so the probe name needs no real
    // executable on disk (which keeps this unit test free of
    // OS-specific absolute paths); it only has to miss the builtin and
    // Lisp-export tables to take the external path.
    let job = materialize_runnable("FOO=bar probe-external-cmd").await;
    let head = job.process.as_deref().expect("process");
    // Construction attaches the prefix to the command itself.
    let JobProcess::Command(cmd) = head else {
        panic!("external prefix must stay a command, got {head:?}");
    };
    assert_eq!(
        cmd.env_overrides,
        vec![("FOO".to_string(), "bar".to_string())]
    );
    let argv = head.command_argv().expect("concrete argv");
    assert_eq!(argv.0, "probe-external-cmd");
}

/// Materialize one input string and return the runnable job.
///
/// Test-only shortcut for the metadata regressions below: they all share
/// the same parse → materialize → unwrap-runnable prologue.
async fn materialize_runnable(input: &str) -> Job {
    fn allow_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::Yes)
    }

    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    let plan = super::super::parse::parse_execution_plan(input, Arc::clone(&env)).expect("plan");
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], allow_all)
        .await
        .expect("materialize")
    {
        MaterializeOutcome::Runnable(materialized) => materialized.job,
        MaterializeOutcome::Rejected(failure) => {
            panic!("expected runnable job for {input:?}, got rejection: {failure:?}")
        }
        MaterializeOutcome::NoCommand(_) => panic!("expected runnable job for {input:?}"),
    }
}

/// An external command keeps its redirection on the process: metadata is
/// attached at construction, never through a generic post-hoc setter.
#[tokio::test]
async fn external_redirect_is_retained_on_command() {
    // Materialization never opens the target, so a static path keeps
    // this unit test free of filesystem side effects.
    let job = materialize_runnable("probe-external-cmd > /tmp/dsh-metadata-probe").await;
    let head = job.process.as_deref().expect("process");
    let JobProcess::Command(cmd) = head else {
        panic!("expected command, got {head:?}");
    };
    assert_eq!(cmd.redirects.len(), 1);
    assert!(cmd.env_overrides.is_empty());
}

/// A builtin keeps its redirection on the process (`dirs` is a safe
/// registry probe: materialization never spawns).
#[tokio::test]
async fn builtin_redirect_is_retained_on_builtin_process() {
    let job = materialize_runnable("dirs > /tmp/dsh-metadata-probe").await;
    let head = job.process.as_deref().expect("process");
    let JobProcess::Builtin(builtin) = head else {
        panic!("expected builtin, got {head:?}");
    };
    assert_eq!(builtin.redirects.len(), 1);
    assert!(builtin.env_overrides.is_empty());
}

/// A synthetic source owns no command metadata: the read-only query
/// stays empty, and no construction API can attach any.
#[test]
fn synthetic_source_redirects_query_stays_empty() {
    let source = JobProcess::SyntheticSource(crate::process::PipelineSourceProcess::new(
        "cached output\n".to_string(),
    ));
    assert!(source.redirects().is_empty());
}

/// One rejected stage rejects the whole pipeline: no partial job with
/// stages 1 and 3 is produced.
#[tokio::test]
async fn mixed_pipeline_with_rejected_middle_stage_is_rejected() {
    fn allow_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::Yes)
    }

    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    let plan = super::super::parse::parse_execution_plan(
        "probe-upstream-cmd | FOO=bar alias | probe-downstream-cmd",
        Arc::clone(&env),
    )
    .expect("plan");
    assert_eq!(plan.lists[0].jobs[0].stages.len(), 3);
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], allow_all)
        .await
        .expect("materialize")
    {
        MaterializeOutcome::Rejected(_) => {}
        MaterializeOutcome::Runnable(_) => {
            panic!("pipeline with a rejected stage must not be runnable")
        }
        MaterializeOutcome::NoCommand(_) => panic!("pipeline is not no-command"),
    }
}

/// Standalone assignments materialize as `NoCommand` without touching the
/// shell: the shared `execute_no_command` applies the value exactly once
/// and reports status 0.
#[tokio::test]
async fn standalone_assignment_is_no_command() {
    use super::super::no_command::{NoCommandExecutionResult, execute_no_command};

    fn allow_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::Yes)
    }

    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    let plan = super::super::parse::parse_execution_plan("FOO=standalone", Arc::clone(&env))
        .expect("plan");
    assert!(plan.lists[0].jobs[0].is_assignment_only());
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
    let no_command = match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], allow_all)
        .await
        .expect("materialize")
    {
        MaterializeOutcome::NoCommand(no_command) => no_command,
        MaterializeOutcome::Runnable(_) => panic!("standalone assignment must not run"),
        MaterializeOutcome::Rejected(failure) => {
            panic!("standalone assignment must not be rejected: {failure:?}")
        }
    };
    // Materialization itself leaves the shell untouched; execution applies.
    assert_eq!(
        shell.environment.read().lookup_variable("FOO").as_deref(),
        None
    );
    assert_eq!(
        no_command.assignments,
        vec![("FOO".to_string(), "standalone".to_string())]
    );
    assert!(no_command.redirects.is_empty());
    assert_eq!(no_command.last_command_substitution_status, None);
    match execute_no_command(&mut shell, &mut ctx, *no_command) {
        NoCommandExecutionResult::Completed(0) => {}
        other => panic!("standalone assignment must complete 0, got {other:?}"),
    }
    assert_eq!(
        shell.environment.read().lookup_variable("FOO").as_deref(),
        Some("standalone")
    );
}

/// A pipeline with an assignment-only stage is runnable: the empty
/// stage keeps its position as an isolated no-command member, nothing
/// is applied to the parent shell, and nothing spawns here.
#[tokio::test]
async fn pipeline_with_assignment_only_stage_is_runnable_without_parent_leak() {
    fn allow_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::Yes)
    }

    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    let plan =
        super::super::parse::parse_execution_plan("FOO=bar | cat", Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists[0].jobs[0].stages.len(), 2);
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    let materialized = match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], allow_all)
        .await
        .expect("materialize")
    {
        MaterializeOutcome::Runnable(materialized) => materialized,
        MaterializeOutcome::Rejected(failure) => {
            panic!("pipeline with an empty stage must be runnable: {failure:?}")
        }
        MaterializeOutcome::NoCommand(_) => panic!("multi-stage job is not no-command"),
    };
    let head = materialized.job.process.as_deref().expect("process");
    assert_eq!(head.stage_count(), 2);
    let JobProcess::NoCommand(no_command) = head else {
        panic!("first stage must be no-command, got {head:?}");
    };
    assert_eq!(
        no_command.assignments,
        vec![("FOO".to_string(), "bar".to_string())]
    );
    assert_eq!(
        shell.environment.read().lookup_variable("FOO").as_deref(),
        None,
        "pipeline assignments must not leak into the parent shell"
    );
}

/// The dry projection must not show a rejected pipeline as a smaller
/// runnable one to SafetyGuard: it errors fail-closed instead.
#[test]
fn dry_materialization_rejects_builtin_prefix_pipeline() {
    let env = crate::environment::Environment::new();
    let shell = Shell::new(env.clone());
    let plan = super::super::parse::parse_execution_plan(
        "FOO=bar alias | probe-downstream-cmd",
        Arc::clone(&env),
    )
    .expect("plan");
    let err = dry_materialize_job(&plan.lists[0].jobs[0], &shell).expect_err("dry must reject");
    assert!(
        err.to_string().contains("not supported for builtins"),
        "unexpected dry error: {err:?}"
    );
}

/// The dry projection keeps an empty pipeline stage in place:
/// `probe-a | FOO=bar | probe-dangerous-c` projects three stages, so
/// SafetyGuard judges the real topology instead of a collapsed
/// `probe-a | probe-dangerous-c`.
#[test]
fn dry_materialization_does_not_collapse_empty_pipeline_stage() {
    let env = crate::environment::Environment::new();
    let shell = Shell::new(env.clone());
    let plan = super::super::parse::parse_execution_plan(
        "probe-a | FOO=bar | probe-dangerous-c",
        Arc::clone(&env),
    )
    .expect("plan");
    assert_eq!(plan.lists[0].jobs[0].stages.len(), 3);
    let job = dry_materialize_job(&plan.lists[0].jobs[0], &shell)
        .expect("dry must project")
        .expect("job");
    let head = job.process.as_deref().expect("process");
    assert_eq!(head.stage_count(), 3);
    let middle = head.next_process().expect("middle stage");
    assert!(
        matches!(middle, JobProcess::NoCommand(_)),
        "middle stage must stay no-command, got {middle:?}"
    );
    assert!(middle.command_argv().is_none());
}

/// A statically empty middle stage (redirection-only, no substitution
/// involved) is runnable on the live path too: it keeps its position
/// with its redirections attached.
#[tokio::test]
async fn pipeline_with_redirect_only_stage_is_runnable_with_redirects() {
    fn allow_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::Yes)
    }

    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    let plan = super::super::parse::parse_execution_plan(
        "probe-upstream-cmd | > /tmp/dsh-no-command-stage-probe | probe-downstream-cmd",
        Arc::clone(&env),
    )
    .expect("plan");
    assert_eq!(plan.lists[0].jobs[0].stages.len(), 3);
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    let materialized = match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], allow_all)
        .await
        .expect("materialize")
    {
        MaterializeOutcome::Runnable(materialized) => materialized,
        MaterializeOutcome::Rejected(failure) => {
            panic!("pipeline with an empty stage must be runnable: {failure:?}")
        }
        MaterializeOutcome::NoCommand(_) => panic!("multi-stage job is not no-command"),
    };
    let head = materialized.job.process.as_deref().expect("process");
    assert_eq!(head.stage_count(), 3);
    let middle = head.next_process().expect("middle stage");
    let JobProcess::NoCommand(no_command) = middle else {
        panic!("middle stage must be no-command, got {middle:?}");
    };
    assert!(no_command.assignments.is_empty());
    assert!(!no_command.redirects.is_empty());
}

/// Redirect and assignment substitutions count as dynamic, not just argv.
#[test]
fn redirect_and_assignment_bodies_are_dynamic() {
    let env = crate::environment::Environment::new();
    let plan =
        super::super::parse::parse_execution_plan("echo hi > $(some-command)", Arc::clone(&env))
            .expect("plan");
    assert!(plan.lists[0].jobs[0].contains_dynamic_expansion());
    let plan =
        super::super::parse::parse_execution_plan("FOO=$(some-command) command", Arc::clone(&env))
            .expect("plan");
    assert!(plan.lists[0].jobs[0].contains_dynamic_expansion());
}
