//! Static job projection for safety checks and tests.
//!
//! The dry path mirrors live materialization without executing anything:
//! no substitution runs, no shell state mutates, and async lists are
//! flattened into one inspection projection (async execution is never
//! started here). Fail closed: malformed input is an error, so callers
//! never judge a parsed prefix while the whole line runs.

use super::materialize::{ExpandedStage, assemble_job};
use super::parse::planned_to_concrete;
use super::plan::{ExecutionPlan, PlannedJob, PlannedRedirectOp};
use super::word_expand::{dry_expand_argument_word, dry_expand_scalar_word};
use crate::process::{Job, Redirect};
use crate::shell::Shell;
use anyhow::Result;

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
    // A synthetic source counts as a stage for dry projection too, so the
    // guard sees the same topology the live path launches (source skipped
    // as safety-neutral). An empty downstream with a source fails closed.
    let source_data = planned.pipeline_source.map(|_| String::new());
    // A single no-command stage carries no argv to authorize: report "no job",
    // as before for standalone assignments.
    if source_data.is_none() && expanded.len() == 1 && expanded[0].argv.is_empty() {
        return Ok(None);
    }
    // Never show a collapsed pipeline to SafetyGuard: a rejected stage
    // surfaces as an error instead of a smaller runnable job (e.g.
    // `FOO=bar alias | dangerous-command` must not become just
    // `dangerous-command`). An empty stage keeps its position as a
    // no-command member of the projected topology, so the guard judges the
    // same stage sequence the live path launches (`A | empty | dangerous-C`
    // must not become just `A | dangerous-C`). The no-command node carries
    // no argv, so `command_argv` skips it exactly like a synthetic source.
    match assemble_job(shell, planned, expanded, 0, source_data) {
        Ok(job) => Ok(Some(job)),
        Err(failure) => anyhow::bail!("{}", failure.message),
    }
}

pub fn dry_materialize_plan(plan: &ExecutionPlan, shell: &Shell) -> Result<Vec<Job>> {
    // Static inspection only: flatten every list's jobs into one
    // projection. Never starts async execution.
    let mut jobs = Vec::new();
    for planned in plan.iter_jobs() {
        if let Some(job) = dry_materialize_job(planned, shell)? {
            jobs.push(job);
        }
    }
    Ok(jobs)
}
