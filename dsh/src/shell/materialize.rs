//! Turn a side-effect-free `ExecutionPlan` into runnable `Job`s.
//!
//! Gating happens before this module is entered. Substitution bodies are
//! expanded here through `super::substitution` (which authorizes each nested
//! body first); the outer job is authorized by the caller once its argv is
//! concrete.

use super::authorize::ConfirmFn;
use super::plan::{PlannedArg, PlannedJob};
use super::substitution::{capture_subshell_plan_stdout, start_process_substitution};
use crate::process::{Job, JobProcess, Redirect, SubshellType};
use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;
use std::future::Future;
use std::pin::Pin;

pub struct MaterializedJob {
    pub job: Job,
    pub had_deferred_evaluation: bool,
}

pub(crate) struct ExpandedStage {
    pub argv: Vec<String>,
    pub redirects: Vec<Redirect>,
    pub env_overrides: Vec<(String, String)>,
}

/// Build one pipeline stage process. `None` means "skip this stage": a
/// `NAME=value` prefix on a builtin, which the shell reports and drops.
fn build_stage_process(
    shell: &Shell,
    argv: Vec<String>,
    redirects: Vec<Redirect>,
    env_overrides: Vec<(String, String)>,
) -> Option<JobProcess> {
    let cmd = argv[0].clone();
    if !env_overrides.is_empty()
        && (dsh_builtin::get_handler(&cmd).is_some() || shell.lisp_engine.borrow().is_export(&cmd))
    {
        eprintln!("dsh: {cmd}: a NAME=value prefix is not supported for builtins");
        return None;
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
    Some(process)
}

fn assemble_job(
    shell: &Shell,
    planned: &PlannedJob,
    expanded: Vec<ExpandedStage>,
    job_id: usize,
) -> Option<Job> {
    let mut job = Job::new(planned.source.clone(), shell.pgid);
    job.job_id = job_id;
    job.foreground = planned.foreground;
    job.capture_output = planned.capture_output;
    job.struct_pipe_exprs = planned.struct_pipe_exprs.clone();
    job.subshell = planned.subshell.clone();
    job.list_op = planned.list_op.clone();
    for mut stage in expanded {
        if stage.argv.is_empty() {
            continue;
        }
        if stage.argv.first().is_some_and(|first| first == "nopty") && stage.argv.len() > 1 {
            stage.argv.remove(0);
            job.disable_pty = true;
        }
        if let Some(process) =
            build_stage_process(shell, stage.argv, stage.redirects, stage.env_overrides)
        {
            job.set_process(process);
        }
    }
    job.has_process().then_some(job)
}

async fn expand_arg(
    shell: &mut Shell,
    ctx: &Context,
    arg: &PlannedArg,
    confirm: ConfirmFn,
    out: &mut Vec<String>,
) -> Result<()> {
    match arg {
        PlannedArg::Literal(value) => out.push(value.clone()),
        PlannedArg::Substitution(subst) => match subst.kind {
            SubshellType::CommandSubstitution => {
                let output = capture_subshell_plan_stdout(shell, ctx, &subst.plan, confirm).await?;
                out.extend(
                    output
                        .split_whitespace()
                        .filter(|part| !part.is_empty())
                        .map(str::to_owned),
                );
            }
            SubshellType::Subshell => {
                let output = capture_subshell_plan_stdout(shell, ctx, &subst.plan, confirm).await?;
                out.extend(output.lines().map(str::to_owned));
            }
            SubshellType::ProcessSubstitution => {
                out.push(start_process_substitution(shell, ctx, &subst.plan, confirm).await?);
            }
            SubshellType::None => {}
        },
    }
    Ok(())
}

pub fn materialize_job<'a>(
    shell: &'a mut Shell,
    ctx: &'a Context,
    planned: &'a PlannedJob,
    confirm: ConfirmFn,
) -> Pin<Box<dyn Future<Output = Result<Option<MaterializedJob>>> + 'a>> {
    Box::pin(async move {
        let mut expanded = Vec::with_capacity(planned.stages.len());
        for stage in &planned.stages {
            let mut argv = Vec::new();
            for arg in &stage.argv {
                expand_arg(shell, ctx, arg, confirm, &mut argv).await?;
            }
            expanded.push(ExpandedStage {
                argv,
                redirects: stage.redirects.clone(),
                env_overrides: stage.env_overrides.clone(),
            });
        }
        // Assignment-only stages apply to the shell, but only now that the job
        // was actually selected for execution. This includes mixed pipelines
        // (`FOO=bar | cat`): the empty stage has no process to carry the
        // assignment, so it is applied to the shell alongside the launch.
        let pending: Vec<(String, String)> = expanded
            .iter()
            .filter(|stage| stage.argv.is_empty())
            .flat_map(|stage| stage.env_overrides.clone())
            .collect();
        let concrete: Vec<ExpandedStage> = expanded
            .into_iter()
            .filter(|stage| !stage.argv.is_empty())
            .collect();
        if !pending.is_empty() {
            let mut env = shell.environment.write();
            for (name, value) in &pending {
                env.set_shell_var(name.clone(), value.clone());
            }
        }
        if concrete.is_empty() {
            return Ok(None);
        }
        let job_id = shell.get_next_job_id();
        let had_deferred = planned.contains_deferred_evaluation();
        let job = assemble_job(shell, planned, concrete, job_id);
        Ok(job.map(|job| MaterializedJob {
            job,
            had_deferred_evaluation: had_deferred,
        }))
    })
}

/// Static materialization for safety checks: no execution, no env mutation.
pub fn dry_materialize_job(planned: &PlannedJob, shell: &Shell) -> Result<Option<Job>> {
    let mut expanded = Vec::with_capacity(planned.stages.len());
    for stage in &planned.stages {
        let mut argv = Vec::new();
        for arg in &stage.argv {
            match arg {
                PlannedArg::Literal(value) => argv.push(value.clone()),
                PlannedArg::Substitution(subst) => argv.push(format!("$({})", subst.source)),
            }
        }
        expanded.push(ExpandedStage {
            argv,
            redirects: stage.redirects.clone(),
            env_overrides: stage.env_overrides.clone(),
        });
    }
    if expanded.iter().all(|stage| stage.argv.is_empty()) {
        return Ok(None);
    }
    let concrete: Vec<ExpandedStage> = expanded
        .into_iter()
        .filter(|stage| !stage.argv.is_empty())
        .collect();
    Ok(assemble_job(shell, planned, concrete, 0))
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
        assert!(plan.jobs[0].contains_deferred_evaluation());
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
}
