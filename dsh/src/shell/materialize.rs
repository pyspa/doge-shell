//! Turn a side-effect-free `ExecutionPlan` into runnable `Job`s.
//!
//! Gating happens before this module is entered. Only the selected
//! [`PlannedJob`] is materialized here: each [`PlannedWord`] is expanded with
//! current shell state through `super::word_expand` (variables, `$?`, tilde,
//! brace/glob, substitutions), redirects and assignments included. The outer
//! job is authorized by the caller once its argv is concrete.

use super::authorize::ConfirmFn;
use super::no_command::NoCommandMaterialization;
use super::parse::planned_to_concrete;
use super::plan::{PlannedJob, PlannedRedirectOp};
use super::substitution::ExecutionResources;
use super::word_expand::{
    ExpansionTrace, dry_expand_argument_word, dry_expand_scalar_word, expand_argument_word,
    expand_assignment_value, expand_redirect_target,
};
use crate::process::{Job, JobProcess, Redirect};
use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;
use std::future::Future;
use std::pin::Pin;

pub struct MaterializedJob {
    pub job: Job,
    pub had_dynamic_expansion: bool,
    /// Process-substitution fds and producer pids created while expanding
    /// this job. Moved into the `Job` before launch; dropped (fds closed,
    /// producers reaped) once every stage is spawned.
    pub resources: ExecutionResources,
}

pub(crate) struct ExpandedStage {
    pub argv: Vec<String>,
    pub redirects: Vec<Redirect>,
    pub env_overrides: Vec<(String, String)>,
    pub last_command_substitution_status: Option<i32>,
}

/// Exit status for a rejected `NAME=value` builtin prefix. This is an
/// ordinary non-zero command failure, matching the syntax-error convention —
/// deliberately not 127 (command-not-found) and not 130 (SIGINT/cancel).
pub const BUILTIN_ENV_PREFIX_EXIT_CODE: i32 = 1;

/// An expected shell-level command refusal: the command was understood but
/// must not run (e.g. a `NAME=value` prefix on a builtin). This is a normal
/// non-zero command result, not an infrastructure error, so evaluators must
/// publish its status and continue the `;`/`&&`/`||` list instead of
/// aborting with `anyhow::Error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMaterializationFailure {
    pub exit_code: i32,
    pub message: String,
}

impl CommandMaterializationFailure {
    fn builtin_env_prefix(cmd: &str) -> Self {
        Self {
            exit_code: BUILTIN_ENV_PREFIX_EXIT_CODE,
            // No trailing newline: `Context::write_stderr` appends exactly one.
            message: format!("dsh: {cmd}: a NAME=value prefix is not supported for builtins"),
        }
    }

    pub(crate) fn pipeline_no_command() -> Self {
        Self {
            exit_code: 1,
            // No trailing newline: `Context::write_stderr` appends exactly one.
            // `dsh:` matches the other user-facing command diagnostics on this
            // path (builtin-prefix refusal, fd errors, command-not-found).
            message: "dsh: pipeline stage expanded to no command".to_string(),
        }
    }
}

/// Explicit materialization result.
///
/// - `Runnable`: an executable command remains after expansion.
/// - `NoCommand`: expansion completed normally but no command name remains
///   (standalone assignment, redirection-only line, or words that expanded
///   to zero fields such as `$(false)`). Carries the assignments,
///   redirections, and last command-substitution status the shared
///   no-command executor needs.
/// - `Rejected`: an understood command was explicitly refused before launch
///   (builtin `NAME=value` prefix, or a pipeline stage that expanded to no
///   command). A normal non-zero command result, never infrastructure error.
///
/// (`Runnable` and `NoCommand` are boxed: the job plus its substitution
/// resources are several hundred bytes, and `Rejected` is tiny.)
pub enum MaterializeOutcome {
    Runnable(Box<MaterializedJob>),
    NoCommand(Box<NoCommandMaterialization>),
    Rejected(CommandMaterializationFailure),
}

/// Build one pipeline stage process. A `NAME=value` prefix on a builtin (or
/// an exported Lisp command, which runs through the same builtin path) is an
/// expected command-level refusal, returned as `Err` so the caller can turn
/// it into a non-zero status without aborting the command list.
fn build_stage_process(
    shell: &Shell,
    argv: Vec<String>,
    redirects: Vec<Redirect>,
    env_overrides: Vec<(String, String)>,
) -> Result<JobProcess, CommandMaterializationFailure> {
    let cmd = argv[0].clone();
    if !env_overrides.is_empty()
        && (dsh_builtin::get_handler(&cmd).is_some() || shell.lisp_engine.borrow().is_export(&cmd))
    {
        return Err(CommandMaterializationFailure::builtin_env_prefix(&cmd));
    }
    let mut process = if let Some(handler) = dsh_builtin::get_handler(&cmd) {
        JobProcess::Builtin(crate::process::BuiltinProcess::new_handler(
            cmd, handler, argv,
        ))
    } else if shell.lisp_engine.borrow().is_export(&cmd) {
        JobProcess::Builtin(crate::process::BuiltinProcess::new(
            cmd,
            dsh_builtin::lisp::run,
            argv,
        ))
    } else if shell.environment.read().lookup(&cmd).is_none()
        && crate::dirs::is_dir(&cmd)
        && let Some(handler) = dsh_builtin::get_handler("cd")
    {
        JobProcess::Builtin(crate::process::BuiltinProcess::new_handler(
            cmd.clone(),
            handler,
            vec!["cd".to_string(), cmd],
        ))
    } else {
        JobProcess::Command(crate::process::Process::new(cmd, argv))
    };
    process.set_redirects(redirects);
    process.set_env_overrides(env_overrides);
    Ok(process)
}

fn assemble_job(
    shell: &Shell,
    planned: &PlannedJob,
    expanded: Vec<ExpandedStage>,
    job_id: usize,
) -> Result<Job, CommandMaterializationFailure> {
    let mut job = Job::new(planned.source.clone(), shell.pgid);
    job.job_id = job_id;
    job.foreground = planned.foreground;
    job.capture_output = planned.capture_output;
    job.struct_pipe_exprs = planned.struct_pipe_exprs.clone();
    job.subshell = planned.subshell.clone();
    job.list_op = planned.list_op.clone();
    for mut stage in expanded {
        // Fail closed: expansion must have ruled out empty stages before this
        // point. Silently dropping one would rewire `A | empty | C` into
        // `A | C`, a different pipeline.
        if stage.argv.is_empty() {
            return Err(CommandMaterializationFailure::pipeline_no_command());
        }
        if stage.argv.first().is_some_and(|first| first == "nopty") && stage.argv.len() > 1 {
            stage.argv.remove(0);
            job.disable_pty = true;
        }
        // Fail fast: one rejected stage rejects the whole pipeline before any
        // process spawns. Dropping just that stage would silently rewire
        // `A | rejected-B | C` into `A | C`.
        let process = build_stage_process(shell, stage.argv, stage.redirects, stage.env_overrides)?;
        job.set_process(process);
    }
    debug_assert!(
        job.has_process(),
        "assemble_job called with no runnable stage"
    );
    Ok(job)
}

async fn expand_redirects(
    shell: &mut Shell,
    ctx: &Context,
    planned: &PlannedJob,
    stage_index: usize,
    confirm: ConfirmFn,
    resources: &mut ExecutionResources,
    trace: &mut ExpansionTrace,
) -> Result<Vec<Redirect>> {
    let mut out = Vec::new();
    for redirect in &planned.stages[stage_index].redirects {
        match &redirect.op {
            PlannedRedirectOp::DupFrom(from) => {
                out.push(Redirect::dup(redirect.fd, *from));
            }
            PlannedRedirectOp::Close => out.push(Redirect::close(redirect.fd)),
            PlannedRedirectOp::ReadFile(word)
            | PlannedRedirectOp::WriteFile(word)
            | PlannedRedirectOp::AppendFile(word)
            | PlannedRedirectOp::BothWrite(word)
            | PlannedRedirectOp::BothAppend(word) => {
                // Target expansion happens once; `&>` forms share it.
                // Substitution bodies inside the target execute as part of
                // `expand_redirect_target`, through the same authorize-then-run
                // path as argv substitutions, and feed the same trace.
                let target =
                    expand_redirect_target(shell, ctx, word, confirm, resources, trace).await?;
                out.extend(planned_to_concrete(redirect, target));
            }
        }
    }
    Ok(out)
}

pub fn materialize_job<'a>(
    shell: &'a mut Shell,
    ctx: &'a Context,
    planned: &'a PlannedJob,
    confirm: ConfirmFn,
) -> Pin<Box<dyn Future<Output = Result<MaterializeOutcome>> + 'a>> {
    Box::pin(async move {
        let mut resources = ExecutionResources::new();
        let mut expanded = Vec::with_capacity(planned.stages.len());
        // Stage order is argv, then assignments, then redirects: the "last"
        // command substitution is the last one in that deterministic order.
        for (stage_index, stage) in planned.stages.iter().enumerate() {
            let mut trace = ExpansionTrace::default();
            let mut argv = Vec::new();
            for word in &stage.argv {
                argv.extend(
                    expand_argument_word(shell, ctx, word, confirm, &mut resources, &mut trace)
                        .await?,
                );
            }
            let mut env_overrides = Vec::with_capacity(stage.env_overrides.len());
            for assignment in &stage.env_overrides {
                let value = expand_assignment_value(
                    shell,
                    ctx,
                    &assignment.value,
                    confirm,
                    &mut resources,
                    &mut trace,
                )
                .await?;
                env_overrides.push((assignment.name.clone(), value));
            }
            let redirects = expand_redirects(
                shell,
                ctx,
                planned,
                stage_index,
                confirm,
                &mut resources,
                &mut trace,
            )
            .await?;
            expanded.push(ExpandedStage {
                argv,
                redirects,
                env_overrides,
                last_command_substitution_status: trace.last_command_substitution_status,
            });
        }
        // Single-stage expansion with no command name is not a refusal and
        // not an empty pipeline: it is a no-command simple command whose
        // assignments, redirections, and substitution status the shared
        // executor handles. Nothing is applied to the shell here;
        // `execute_no_command` owns those side effects so the top-level and
        // helper evaluators share one semantics.
        if expanded.len() == 1 && expanded[0].argv.is_empty() {
            let stage = expanded.pop().expect("single stage");
            return Ok(MaterializeOutcome::NoCommand(Box::new(
                NoCommandMaterialization {
                    assignments: stage.env_overrides,
                    redirects: stage.redirects,
                    last_command_substitution_status: stage.last_command_substitution_status,
                    resources,
                },
            )));
        }
        // Multi-stage pipelines never drop or rewire an empty stage
        // (`A | empty | C` must not become `A | C`), never leak its
        // assignments into the parent shell, and never launch a subset of
        // stages: fail closed as an explicit command-level failure before
        // any process spawns.
        if expanded.iter().any(|stage| stage.argv.is_empty()) {
            return Ok(MaterializeOutcome::Rejected(
                CommandMaterializationFailure::pipeline_no_command(),
            ));
        }
        let job_id = shell.get_next_job_id();
        let had_dynamic = planned.contains_dynamic_expansion();
        match assemble_job(shell, planned, expanded, job_id) {
            Ok(job) => Ok(MaterializeOutcome::Runnable(Box::new(MaterializedJob {
                job,
                had_dynamic_expansion: had_dynamic,
                resources,
            }))),
            Err(failure) => Ok(MaterializeOutcome::Rejected(failure)),
        }
    })
}

/// Static materialization for safety checks: no execution, no env mutation.
pub fn dry_materialize_job(planned: &PlannedJob, shell: &Shell) -> Result<Option<Job>> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut expanded = Vec::with_capacity(planned.stages.len());
    for stage in &planned.stages {
        let mut argv = Vec::new();
        for word in &stage.argv {
            argv.extend(dry_expand_argument_word(word, shell, &cwd));
        }
        let mut env_overrides = Vec::with_capacity(stage.env_overrides.len());
        for assignment in &stage.env_overrides {
            env_overrides.push((
                assignment.name.clone(),
                dry_expand_scalar_word(&assignment.value, shell),
            ));
        }
        let mut redirects = Vec::new();
        for redirect in &stage.redirects {
            match &redirect.op {
                PlannedRedirectOp::DupFrom(from) => {
                    redirects.push(Redirect::dup(redirect.fd, *from));
                }
                PlannedRedirectOp::Close => redirects.push(Redirect::close(redirect.fd)),
                PlannedRedirectOp::ReadFile(word)
                | PlannedRedirectOp::WriteFile(word)
                | PlannedRedirectOp::AppendFile(word)
                | PlannedRedirectOp::BothWrite(word)
                | PlannedRedirectOp::BothAppend(word) => {
                    let fields = dry_expand_argument_word(word, shell, &cwd);
                    if fields.len() != 1 {
                        anyhow::bail!(
                            "ambiguous redirect: '{}' expands to {} fields",
                            word.source,
                            fields.len()
                        );
                    }
                    redirects.extend(planned_to_concrete(
                        redirect,
                        fields.into_iter().next().expect("one field"),
                    ));
                }
            }
        }
        expanded.push(ExpandedStage {
            argv,
            redirects,
            env_overrides,
            last_command_substitution_status: None,
        });
    }
    // A single no-command stage carries no argv to authorize: report "no job",
    // as before for standalone assignments.
    if expanded.len() == 1 && expanded[0].argv.is_empty() {
        return Ok(None);
    }
    // Fail closed: never show a collapsed pipeline to SafetyGuard. Both a
    // rejected stage and an empty stage must surface as an error instead of
    // a smaller runnable job (e.g. `A | empty | dangerous-C` must not become
    // just `A | dangerous-C`, and `FOO=bar alias | dangerous-command` must
    // not become just `dangerous-command`).
    if expanded.iter().any(|stage| stage.argv.is_empty()) {
        anyhow::bail!("dsh: pipeline stage expanded to no command");
    }
    match assemble_job(shell, planned, expanded, 0) {
        Ok(job) => Ok(Some(job)),
        Err(failure) => anyhow::bail!("{}", failure.message),
    }
}

pub fn dry_materialize_plan(plan: &super::plan::ExecutionPlan, shell: &Shell) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();
    for planned in &plan.jobs {
        if let Some(job) = dry_materialize_job(planned, shell)? {
            jobs.push(job);
        }
    }
    Ok(jobs)
}

#[cfg(test)]
mod tests {
    use super::super::authorize::is_authorization_cancelled;
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
        let plan =
            super::super::parse::parse_execution_plan(&input, Arc::clone(&shell.environment))
                .expect("plan");
        assert_eq!(plan.jobs.len(), 1);
        assert!(plan.jobs[0].contains_dynamic_expansion());
        let ctx = Context::new_safe(shell.pid, shell.pgid, false);
        match materialize_job(&mut shell, &ctx, &plan.jobs[0], deny_all).await {
            Err(err) => assert!(
                is_authorization_cancelled(&err),
                "nested denial must surface as cancellation, got {err:?}"
            ),
            Ok(_) => panic!("nested dangerous body must not materialize"),
        }
        assert!(victim.exists(), "denied nested body must not run");
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
        assert!(plan.jobs[0].contains_dynamic_expansion());
        let ctx = Context::new_safe(shell.pid, shell.pgid, false);
        let materialized = match materialize_job(&mut shell, &ctx, &plan.jobs[0], allow_all)
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
            .command_argv();
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
        let plan = super::super::parse::parse_execution_plan("FOO=bar alias", Arc::clone(&env))
            .expect("plan");
        let ctx = Context::new_safe(shell.pid, shell.pgid, false);
        match materialize_job(&mut shell, &ctx, &plan.jobs[0], allow_all)
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
        match materialize_job(&mut shell, &ctx, &plan.jobs[0], allow_all)
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
        fn allow_all(_: &str) -> Result<ConfirmationAction> {
            Ok(ConfirmationAction::Yes)
        }

        let env = crate::environment::Environment::new();
        let mut shell = Shell::new(env.clone());
        // Materialization never spawns, so the probe name needs no real
        // executable on disk (which keeps this unit test free of
        // OS-specific absolute paths); it only has to miss the builtin and
        // Lisp-export tables to take the external path.
        let plan = super::super::parse::parse_execution_plan(
            "FOO=bar probe-external-cmd",
            Arc::clone(&env),
        )
        .expect("plan");
        let ctx = Context::new_safe(shell.pid, shell.pgid, false);
        match materialize_job(&mut shell, &ctx, &plan.jobs[0], allow_all)
            .await
            .expect("materialize")
        {
            MaterializeOutcome::Runnable(materialized) => {
                let argv = materialized
                    .job
                    .process
                    .as_ref()
                    .expect("process")
                    .command_argv();
                assert_eq!(argv.0, "probe-external-cmd");
            }
            MaterializeOutcome::Rejected(failure) => {
                panic!("external prefix must not be rejected: {failure:?}")
            }
            MaterializeOutcome::NoCommand(_) => panic!("external prefix is not no-command"),
        }
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
        assert_eq!(plan.jobs[0].stages.len(), 3);
        let ctx = Context::new_safe(shell.pid, shell.pgid, false);
        match materialize_job(&mut shell, &ctx, &plan.jobs[0], allow_all)
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
        assert!(plan.jobs[0].is_assignment_only());
        let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
        let no_command = match materialize_job(&mut shell, &ctx, &plan.jobs[0], allow_all)
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

    /// A pipeline with an assignment-only stage fails closed: no stage is
    /// dropped, nothing is applied to the parent shell, and nothing spawns.
    #[tokio::test]
    async fn pipeline_with_assignment_only_stage_is_rejected_without_parent_leak() {
        fn allow_all(_: &str) -> Result<ConfirmationAction> {
            Ok(ConfirmationAction::Yes)
        }

        let env = crate::environment::Environment::new();
        let mut shell = Shell::new(env.clone());
        let plan = super::super::parse::parse_execution_plan("FOO=bar | cat", Arc::clone(&env))
            .expect("plan");
        assert_eq!(plan.jobs[0].stages.len(), 2);
        let ctx = Context::new_safe(shell.pid, shell.pgid, false);
        match materialize_job(&mut shell, &ctx, &plan.jobs[0], allow_all)
            .await
            .expect("materialize")
        {
            MaterializeOutcome::Rejected(_) => {}
            MaterializeOutcome::Runnable(_) => {
                panic!("pipeline with an empty stage must not be runnable")
            }
            MaterializeOutcome::NoCommand(_) => panic!("multi-stage job is not no-command"),
        }
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
        let err = dry_materialize_job(&plan.jobs[0], &shell).expect_err("dry must reject");
        assert!(
            err.to_string().contains("not supported for builtins"),
            "unexpected dry error: {err:?}"
        );
    }

    /// The dry projection must not collapse an empty pipeline stage either:
    /// `probe-a | FOO=bar | probe-c` errors fail-closed instead of showing a
    /// smaller runnable pipeline to SafetyGuard.
    #[test]
    fn dry_materialization_does_not_collapse_empty_pipeline_stage() {
        let env = crate::environment::Environment::new();
        let shell = Shell::new(env.clone());
        let plan = super::super::parse::parse_execution_plan(
            "probe-a | FOO=bar | probe-dangerous-c",
            Arc::clone(&env),
        )
        .expect("plan");
        assert_eq!(plan.jobs[0].stages.len(), 3);
        let err = dry_materialize_job(&plan.jobs[0], &shell).expect_err("dry must fail closed");
        assert!(
            err.to_string().contains("no command"),
            "unexpected dry error: {err:?}"
        );
    }

    /// A statically empty middle stage (redirection-only, no substitution
    /// involved) fails closed on the live path too: no launch, no leak.
    #[tokio::test]
    async fn pipeline_with_redirect_only_stage_is_rejected_without_launch() {
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
        assert_eq!(plan.jobs[0].stages.len(), 3);
        let ctx = Context::new_safe(shell.pid, shell.pgid, false);
        match materialize_job(&mut shell, &ctx, &plan.jobs[0], allow_all)
            .await
            .expect("materialize")
        {
            MaterializeOutcome::Rejected(failure) => {
                assert_ne!(failure.exit_code, 0);
                assert!(failure.message.contains("no command"));
            }
            MaterializeOutcome::Runnable(_) => {
                panic!("pipeline with an empty stage must not be runnable")
            }
            MaterializeOutcome::NoCommand(_) => panic!("multi-stage job is not no-command"),
        }
    }

    /// Redirect and assignment substitutions count as dynamic, not just argv.
    #[test]
    fn redirect_and_assignment_bodies_are_dynamic() {
        let env = crate::environment::Environment::new();
        let plan = super::super::parse::parse_execution_plan(
            "echo hi > $(some-command)",
            Arc::clone(&env),
        )
        .expect("plan");
        assert!(plan.jobs[0].contains_dynamic_expansion());
        let plan = super::super::parse::parse_execution_plan(
            "FOO=$(some-command) command",
            Arc::clone(&env),
        )
        .expect("plan");
        assert!(plan.jobs[0].contains_dynamic_expansion());
    }
}
