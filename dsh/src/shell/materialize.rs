//! Turn a side-effect-free `ExecutionPlan` into runnable `Job`s.
//!
//! Gating happens before this module is entered. Only the selected
//! [`PlannedJob`] is materialized here: each [`PlannedWord`] is expanded with
//! current shell state through `super::word_expand` (variables, `$?`, tilde,
//! brace/glob, substitutions), redirects and assignments included. The outer
//! job is authorized by the caller once its argv is concrete.

use super::authorize::ConfirmFn;
use super::parse::planned_to_concrete;
use super::plan::{PlannedJob, PlannedRedirectOp};
use super::word_expand::{
    dry_expand_argument_word, dry_expand_scalar_word, expand_argument_word,
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

async fn expand_redirects(
    shell: &mut Shell,
    ctx: &Context,
    planned: &PlannedJob,
    stage_index: usize,
    confirm: ConfirmFn,
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
                // path as argv substitutions.
                let target = expand_redirect_target(shell, ctx, word, confirm).await?;
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
) -> Pin<Box<dyn Future<Output = Result<Option<MaterializedJob>>> + 'a>> {
    Box::pin(async move {
        let mut expanded = Vec::with_capacity(planned.stages.len());
        for (stage_index, stage) in planned.stages.iter().enumerate() {
            let mut argv = Vec::new();
            for word in &stage.argv {
                argv.extend(expand_argument_word(shell, ctx, word, confirm).await?);
            }
            let mut env_overrides = Vec::with_capacity(stage.env_overrides.len());
            for assignment in &stage.env_overrides {
                let value = expand_assignment_value(shell, ctx, &assignment.value, confirm).await?;
                env_overrides.push((assignment.name.clone(), value));
            }
            let redirects = expand_redirects(shell, ctx, planned, stage_index, confirm).await?;
            expanded.push(ExpandedStage {
                argv,
                redirects,
                env_overrides,
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
        let had_dynamic = planned.contains_dynamic_expansion();
        let job = assemble_job(shell, planned, concrete, job_id);
        Ok(job.map(|job| MaterializedJob {
            job,
            had_dynamic_expansion: had_dynamic,
        }))
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
        let materialized = materialize_job(&mut shell, &ctx, &plan.jobs[0], allow_all)
            .await
            .expect("materialize")
            .expect("job");
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
