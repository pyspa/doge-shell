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
use super::plan::{PlannedCommand, PlannedJob, PlannedRedirectOp};
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
    pub execution_environment: crate::process::stage_environment::StageEnvironment,
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
    execution_environment: crate::process::stage_environment::StageEnvironment,
) -> JobProcess {
    match process {
        StageProcess::Builtin(mut inner) => {
            inner.stage_environment = execution_environment;
            JobProcess::Builtin(inner.with_execution_metadata(redirects, env_overrides))
        }
        StageProcess::Command(mut inner) => {
            inner.stage_environment = execution_environment;
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
    execution_environment: crate::process::stage_environment::StageEnvironment,
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
        && execution_environment
            .environment(&shell.environment)
            .read()
            .lookup_with_path_override(
                &cmd,
                env_overrides
                    .iter()
                    .rev()
                    .find(|(name, _)| name == "PATH")
                    .map(|(_, value)| value.as_str()),
            )
            .is_none()
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
    Ok(finish_stage_process(
        stage,
        redirects,
        env_overrides,
        execution_environment,
    ))
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
                )
                .with_stage_environment(stage.execution_environment),
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
        let process = build_stage_process(
            shell,
            stage.argv,
            stage.redirects,
            stage.env_overrides,
            stage.execution_environment,
        )?;
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
    shell: &mut impl super::expansion_host::ExpansionHost,
    ctx: &Context,
    stage: &PlannedCommand,
    confirm: ConfirmFn,
    resources: &mut ExecutionResources,
    trace: &mut ExpansionTrace,
) -> Result<Vec<Redirect>> {
    let mut out = Vec::new();
    for redirect in &stage.redirects {
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

async fn expand_stage(
    host: &mut super::expansion_host::StageExpansionHost<'_>,
    ctx: &Context,
    stage: &PlannedCommand,
    confirm: ConfirmFn,
    resources: &mut ExecutionResources,
    isolated: bool,
) -> Result<ExpandedStage> {
    let mut trace = ExpansionTrace::default();
    let mut argv = Vec::new();
    for word in &stage.argv {
        argv.extend(expand_argument_word(host, ctx, word, confirm, resources, &mut trace).await?);
    }
    let mut env_overrides = Vec::with_capacity(stage.env_overrides.len());
    for assignment in &stage.env_overrides {
        let value =
            expand_assignment_value(host, ctx, &assignment.value, confirm, resources, &mut trace)
                .await?;
        env_overrides.push((assignment.name.clone(), value));
    }
    let redirects = expand_redirects(host, ctx, stage, confirm, resources, &mut trace).await?;
    let execution_environment = if isolated {
        crate::process::stage_environment::StageEnvironment::Isolated(Box::new(
            crate::environment::child_snapshot::ChildShellSnapshot::capture(
                &host.environment.read(),
            ),
        ))
    } else {
        Default::default()
    };
    Ok(ExpandedStage {
        execution_environment,
        argv,
        redirects,
        env_overrides,
        last_command_substitution_status: trace.last_command_substitution_status,
    })
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
        let isolated = planned.stages.len() + usize::from(planned.pipeline_source.is_some()) >= 2;
        // Each stage starts from the same parent state; the runtime stays borrowed.
        let initial = isolated.then(|| {
            crate::environment::Environment::isolated_expansion(&shell.environment.read())
        });
        // Stage order is argv, then assignments, then redirects: the "last"
        // command substitution is the last one in that deterministic order.
        for stage in &planned.stages {
            let environment = match &initial {
                Some(initial) => {
                    crate::environment::Environment::isolated_expansion(&initial.read())
                }
                None => shell.environment.clone(),
            };
            let mut host = super::expansion_host::StageExpansionHost::new(shell, environment);
            expanded.push(
                expand_stage(&mut host, ctx, stage, confirm, &mut resources, isolated).await?,
            );
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
mod isolation_tests;
#[cfg(test)]
mod tests;
