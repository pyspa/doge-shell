use super::*;
use crate::environment::{Environment, child_snapshot::ChildShellSnapshot};
use crate::process::stage_environment::StageEnvironment;
use crate::repl::confirmation::ConfirmationAction;
use std::sync::Arc;
fn allow(_: &str) -> Result<ConfirmationAction> {
    Ok(ConfirmationAction::Yes)
}
fn shell() -> Shell {
    let env = Environment::new();
    env.write()
        .set_and_export_shell_var("DOGESH_ISO_X".into(), "1".into());
    Shell::new(env)
}
async fn materialize(shell: &mut Shell, input: &str) -> Result<MaterializeOutcome> {
    let plan = super::super::parse::parse_execution_plan(input, shell.environment.clone())?;
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    materialize_job(shell, &ctx, &plan.lists[0].jobs[0], allow).await
}
fn snapshot(process: &JobProcess) -> &ChildShellSnapshot {
    let metadata = match process {
        JobProcess::Command(p) => &p.stage_environment,
        JobProcess::Builtin(p) => &p.stage_environment,
        JobProcess::NoCommand(p) => &p.stage_environment,
        _ => panic!("unexpected stage"),
    };
    match metadata {
        StageEnvironment::Isolated(s) => s,
        _ => panic!("expected isolation"),
    }
}
#[tokio::test]
async fn stages_share_order_but_not_state_and_carry_exported_envp() {
    let mut shell = shell();
    let MaterializeOutcome::Runnable(job) = materialize(
        &mut shell,
        "probe-cmd \"$((DOGESH_ISO_X=9))\" \"$DOGESH_ISO_X\" | probe-cmd \"$DOGESH_ISO_X\"",
    )
    .await
    .unwrap() else {
        panic!("runnable")
    };
    let first = job.job.process.as_deref().unwrap();
    let second = first.next_process().unwrap();
    assert_eq!(first.command_argv().unwrap().1, &["9", "9"]);
    assert_eq!(second.command_argv().unwrap().1, &["1"]);
    assert_eq!(snapshot(first).variables["DOGESH_ISO_X"], "9");
    assert_eq!(snapshot(second).variables["DOGESH_ISO_X"], "1");
    let JobProcess::Command(p) = first else {
        panic!("command")
    };
    let prepared = p
        .prepare_execution(p.stage_environment.environment(&shell.environment))
        .unwrap();
    assert!(
        prepared
            .envp
            .iter()
            .any(|v| v.as_bytes() == b"DOGESH_ISO_X=9")
    );
    assert_eq!(
        shell
            .environment
            .read()
            .lookup_variable("DOGESH_ISO_X")
            .as_deref(),
        Some("1")
    );
    assert!(!format!("{:?}", p.stage_environment).contains("DOGESH_ISO_X"));
}
#[tokio::test]
async fn builtin_and_no_command_receive_post_expansion_snapshot() {
    let mut shell = shell();
    for input in [
        "export \"$((DOGESH_ISO_X=9))\" | cat",
        "DOGESH_ISO_A=$((DOGESH_ISO_X=9)) | cat",
    ] {
        let MaterializeOutcome::Runnable(job) = materialize(&mut shell, input).await.unwrap()
        else {
            panic!("runnable")
        };
        assert_eq!(
            snapshot(job.job.process.as_deref().unwrap()).variables["DOGESH_ISO_X"],
            "9"
        );
        assert_eq!(
            shell
                .environment
                .read()
                .lookup_variable("DOGESH_ISO_X")
                .as_deref(),
            Some("1")
        );
    }
}
#[tokio::test]
async fn fatal_errors_leave_parent_values_export_path_and_ai_unchanged() {
    let mut shell = shell();
    shell
        .environment
        .write()
        .set_shell_var("PATH".into(), String::new());
    let before = ChildShellSnapshot::capture(&shell.environment.read());
    let language = shell
        .environment
        .read()
        .integration_state
        .response_language
        .clone();
    let model = shell
        .environment
        .read()
        .integration_state
        .chat_model
        .clone();
    let client = shell.environment.read().integration_state.ai_client.clone();
    for error in ["$((1/0))", "${DOGESH_ISO_MISSING:?fatal}"] {
        let input = format!(
            "probe-cmd $((DOGESH_ISO_X=9)) ${{PATH:=iso-bin}} ${{AI_MESSAGE_LANG:=iso}} ${{AI_CHAT_MODEL:=iso}} {error} | cat"
        );
        let err = match materialize(&mut shell, &input).await {
            Err(err) => err,
            Ok(_) => panic!("fatal expansion must fail"),
        };
        if error.starts_with("$((") {
            assert!(super::super::arithmetic::is_arithmetic_expansion_error(
                &err
            ));
        } else {
            assert!(super::super::parameter_expand::is_parameter_expansion_error(&err));
        }
        assert!(
            ChildShellSnapshot::capture(&shell.environment.read()) == before,
            "parent logical state changed on fatal expansion"
        );
        assert!(Arc::ptr_eq(
            &language,
            &shell.environment.read().integration_state.response_language
        ));
        assert!(Arc::ptr_eq(
            &model,
            &shell.environment.read().integration_state.chat_model
        ));
        assert!(Arc::ptr_eq(
            &client,
            &shell.environment.read().integration_state.ai_client
        ));
    }
}
#[tokio::test]
async fn whole_pipeline_refusal_preserves_parent_and_live_lisp_preflight() {
    let mut shell = shell();
    shell
        .lisp_engine
        .borrow()
        .run("(fn iso-exported () 1)")
        .unwrap();
    for tail in ["DOGESH_ISO_PREFIX=x alias", "jobs", "iso-exported"] {
        let input = format!("probe-cmd $((DOGESH_ISO_X=9)) | {tail}");
        assert!(matches!(
            materialize(&mut shell, &input).await.unwrap(),
            MaterializeOutcome::Rejected(_)
        ));
        assert_eq!(
            shell
                .environment
                .read()
                .lookup_variable("DOGESH_ISO_X")
                .as_deref(),
            Some("1")
        );
    }
}
#[tokio::test]
async fn single_no_command_retains_updates_and_empty_pipeline_keeps_three_stages() {
    let mut shell = shell();
    assert!(matches!(
        materialize(&mut shell, "DOGESH_ISO_A=$((DOGESH_ISO_X=9))")
            .await
            .unwrap(),
        MaterializeOutcome::NoCommand(_)
    ));
    assert_eq!(
        shell
            .environment
            .read()
            .lookup_variable("DOGESH_ISO_X")
            .as_deref(),
        Some("9")
    );
    let MaterializeOutcome::Runnable(job) =
        materialize(&mut shell, "probe-a | $DOGESH_ISO_UNSET | probe-c")
            .await
            .unwrap()
    else {
        panic!("runnable")
    };
    let head = job.job.process.as_deref().unwrap();
    assert_eq!(head.stage_count(), 3);
    assert!(matches!(
        head.next_process(),
        Some(JobProcess::NoCommand(_))
    ));
}
#[tokio::test]
async fn dry_path_never_creates_snapshot_or_runs_updates() {
    let shell = shell();
    let plan = super::super::parse::parse_execution_plan(
        "probe $((DOGESH_ISO_X=9)) | cat",
        shell.environment.clone(),
    )
    .unwrap();
    let job = super::super::dry_materialize::dry_materialize_job(&plan.lists[0].jobs[0], &shell)
        .unwrap()
        .unwrap();
    let JobProcess::Command(p) = job.process.as_deref().unwrap() else {
        panic!("command")
    };
    assert_eq!(p.stage_environment, StageEnvironment::Current);
    assert_eq!(
        shell
            .environment
            .read()
            .lookup_variable("DOGESH_ISO_X")
            .as_deref(),
        Some("1")
    );
}

#[tokio::test]
async fn final_argv_denial_after_updates_leaves_parent_unchanged() {
    use super::super::authorize::{AuthorizationDecision, authorize_job_with};
    fn deny(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::No)
    }
    let mut shell = shell();
    let before = ChildShellSnapshot::capture(&shell.environment.read());
    let MaterializeOutcome::Runnable(job) = materialize(
        &mut shell,
        "probe-cmd $((DOGESH_ISO_X=9)) | rm -rf iso-victim",
    )
    .await
    .unwrap() else {
        panic!("runnable")
    };
    assert!(matches!(
        authorize_job_with(&mut shell, &job.job, true, deny).unwrap(),
        AuthorizationDecision::Deny
    ));
    assert!(ChildShellSnapshot::capture(&shell.environment.read()) == before);
    assert!(job.job.process.as_deref().unwrap().get_pid().is_none());
}

#[tokio::test]
async fn nested_strict_denial_after_updates_leaves_parent_unchanged() {
    let mut shell = shell();
    *shell.environment.read().policy_state.safety_level.write() =
        crate::safety::SafetyLevel::Strict;
    let before = ChildShellSnapshot::capture(&shell.environment.read());
    let error = match materialize(
        &mut shell,
        "probe $((DOGESH_ISO_X=9)) $(echo must-deny) | cat",
    )
    .await
    {
        Err(err) => err,
        Ok(_) => panic!("strict helper body must be denied"),
    };
    assert!(super::super::authorize::is_authorization_cancelled(&error));
    assert!(ChildShellSnapshot::capture(&shell.environment.read()) == before);
}
