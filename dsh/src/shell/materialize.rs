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
use super::process_substitution::ExecutionResources;
use super::word_expand::{
    ExpansionTrace, expand_argument_word, expand_assignment_value, expand_redirect_target,
};
use crate::process::{BuiltinProcess, Job, JobProcess, Process, Redirect};
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

    pub(crate) fn pipeline_builtin_requires_parent(name: &str) -> Self {
        Self {
            exit_code: 1,
            message: format!(
                "dogesh: {name}: cannot run in a pipeline (needs the live shell session)"
            ),
        }
    }

    pub(crate) fn background_builtin_requires_parent(name: &str) -> Self {
        Self {
            exit_code: 1,
            message: format!(
                "dogesh: {name}: cannot run in background (needs the live shell session)"
            ),
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
///   (builtin `NAME=value` prefix). A normal non-zero command result, never
///   infrastructure error.
///
/// (`Runnable` and `NoCommand` are boxed: the job plus its substitution
/// resources are several hundred bytes, and `Rejected` is tiny.)
pub enum MaterializeOutcome {
    Runnable(Box<MaterializedJob>),
    NoCommand(Box<NoCommandMaterialization>),
    Rejected(CommandMaterializationFailure),
}

/// A stage that owns command-scoped execution metadata.
///
/// `build_stage_process` dispatches to exactly one of these concrete types
/// and attaches the expanded redirects/env overrides at the single
/// `finish_stage_process` site. A future dispatch arm must yield one of
/// these two types, so it cannot silently skip the metadata attach. This is
/// deliberately not a generic `JobProcess` setter: synthetic sources and
/// async-list outer nodes are not constructible here by type.
enum StageProcess {
    Builtin(BuiltinProcess),
    Command(Process),
}

fn finish_stage_process(
    process: StageProcess,
    redirects: Vec<Redirect>,
    env_overrides: Vec<(String, String)>,
) -> JobProcess {
    match process {
        StageProcess::Builtin(inner) => {
            JobProcess::Builtin(inner.with_execution_metadata(redirects, env_overrides))
        }
        StageProcess::Command(inner) => {
            JobProcess::Command(inner.with_execution_metadata(redirects, env_overrides))
        }
    }
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
    let stage = if let Some(handler) = dsh_builtin::get_handler(&cmd) {
        StageProcess::Builtin(BuiltinProcess::new_handler(cmd, handler, argv))
    } else if shell.lisp_engine.borrow().is_export(&cmd) {
        StageProcess::Builtin(BuiltinProcess::new(cmd, dsh_builtin::lisp::run, argv))
    } else if crate::dirs::is_dir(&cmd)
        && shell.environment.read().lookup(&cmd).is_none()
        && let Some(handler) = dsh_builtin::get_handler("cd")
    {
        // Dispatch identity is `cd`; `Job.cmd` keeps the user-facing path.
        StageProcess::Builtin(BuiltinProcess::new_handler(
            "cd".to_string(),
            handler,
            vec!["cd".to_string(), cmd],
        ))
    } else {
        StageProcess::Command(Process::new(cmd, argv))
    };
    Ok(finish_stage_process(stage, redirects, env_overrides))
}

pub(crate) fn assemble_job(
    shell: &Shell,
    planned: &PlannedJob,
    expanded: Vec<ExpandedStage>,
    job_id: usize,
    pipeline_source_data: Option<String>,
) -> Result<Job, CommandMaterializationFailure> {
    let mut job = Job::new(planned.source.clone(), shell.pgid);
    job.job_id = job_id;
    job.capture_output = planned.capture_output;
    job.struct_pipe_exprs = planned.struct_pipe_exprs.clone();
    job.subshell = planned.subshell.clone();
    job.list_op = planned.list_op.clone();
    if let Some(data) = pipeline_source_data {
        job.set_process(JobProcess::SyntheticSource(
            crate::process::PipelineSourceProcess::new(data),
        ));
    }
    for mut stage in expanded {
        // A runtime-expanded stage with no command name is still a real
        // pipeline stage: it keeps its position as an isolated no-command
        // helper child. Silently dropping it would rewire `A | empty | C`
        // into `A | C`, a different pipeline; running its assignments in
        // the parent shell would break pipeline isolation.
        if stage.argv.is_empty() {
            job.set_process(JobProcess::NoCommand(
                crate::process::NoCommandProcess::new(
                    stage.env_overrides,
                    stage.redirects,
                    stage.last_command_substitution_status,
                ),
            ));
            continue;
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
    super::pipeline_isolation::reject_session_bound_pipeline(&job, shell)?;
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
        // A synthetic source never forms a no-command job on its own: with
        // a single empty downstream the job is a two-stage pipeline
        // (`SyntheticSource | NoCommand`), never a collapsed stage.
        let source_data = planned
            .pipeline_source
            .map(|_| super::pipeline_isolation::smart_pipe_source_data(shell));
        // Single-stage expansion with no command name is not a refusal and
        // not an empty pipeline: it is a no-command simple command whose
        // assignments, redirections, and substitution status the shared
        // executor handles. Nothing is applied to the shell here;
        // `execute_no_command` owns those side effects so the top-level and
        // helper evaluators share one semantics.
        if source_data.is_none() && expanded.len() == 1 && expanded[0].argv.is_empty() {
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
        let job_id = shell.get_next_job_id();
        let had_dynamic = planned.contains_dynamic_expansion();
        match assemble_job(shell, planned, expanded, job_id, source_data) {
            Ok(job) => Ok(MaterializeOutcome::Runnable(Box::new(MaterializedJob {
                job,
                had_dynamic_expansion: had_dynamic,
                resources,
            }))),
            Err(failure) => Ok(MaterializeOutcome::Rejected(failure)),
        }
    })
}

#[cfg(test)]
mod tests {
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
        let plan =
            super::super::parse::parse_execution_plan(&input, Arc::clone(&shell.environment))
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
        let materialized =
            match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], allow_all)
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
        let plan = super::super::parse::parse_execution_plan("FOO=bar alias", Arc::clone(&env))
            .expect("plan");
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
        let plan =
            super::super::parse::parse_execution_plan(input, Arc::clone(&env)).expect("plan");
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
        let plan = super::super::parse::parse_execution_plan("FOO=bar | cat", Arc::clone(&env))
            .expect("plan");
        assert_eq!(plan.lists[0].jobs[0].stages.len(), 2);
        let ctx = Context::new_safe(shell.pid, shell.pgid, false);
        let materialized =
            match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], allow_all)
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
        let materialized =
            match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], allow_all)
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
        let plan = super::super::parse::parse_execution_plan(
            "echo hi > $(some-command)",
            Arc::clone(&env),
        )
        .expect("plan");
        assert!(plan.lists[0].jobs[0].contains_dynamic_expansion());
        let plan = super::super::parse::parse_execution_plan(
            "FOO=$(some-command) command",
            Arc::clone(&env),
        )
        .expect("plan");
        assert!(plan.lists[0].jobs[0].contains_dynamic_expansion());
    }
}
