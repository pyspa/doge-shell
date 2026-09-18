//! Side-effect-free shell planning: pest pairs become an `ExecutionPlan`.
//!
//! This module never spawns processes, allocates pipes, touches the working
//! directory, mutates the environment, or asks the safety guard anything. It
//! only reads the environment through `parse_with_expansion` (alias, tilde,
//! variable, brace, glob) and records `$(...)` / `<(...)` / `(...)` bodies as
//! deferred `PlannedSubstitution` nodes for the materializer to evaluate after
//! `&&`/`||` gating and authorization.

use super::plan::{ExecutionPlan, PlannedArg, PlannedCommand, PlannedJob, PlannedSubstitution};
use super::struct_pipe;
use crate::environment::Environment;
use crate::parser::{self, Rule, ShellParser};
use crate::process::{ListOp, Redirect, SubshellType};
use anyhow::{Context as _, Result};
use nix::libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use parking_lot::RwLock;
use pest::Parser as _;
use pest::iterators::Pair;
use std::os::unix::io::RawFd;
use std::sync::Arc;
use tracing::{debug, warn};

/// Pure parse context: pipeline flags only, no shell handle.
#[derive(Debug)]
pub struct ParseContext {
    pub foreground: bool,
    pub subshell: bool,
    pub proc_subst: bool,
}

impl ParseContext {
    pub fn new(foreground: bool) -> Self {
        Self {
            foreground,
            subshell: false,
            proc_subst: false,
        }
    }
}

/// Parse `input` into a side-effect-free plan.
///
/// Only `environment` is read (for the pre-expansion pass). No command is
/// executed and no shell state is mutated.
pub fn parse_execution_plan(
    input: &str,
    environment: Arc<RwLock<Environment>>,
) -> Result<ExecutionPlan> {
    let (input_cow, pairs_opt) = parser::parse_with_expansion(input, environment)?;

    let mut pairs = if let Some(pairs) = pairs_opt {
        pairs
    } else {
        ShellParser::parse(Rule::commands, &input_cow).map_err(|e| anyhow::anyhow!(e))?
    };

    let mut ctx = ParseContext::new(true);
    let Some(pair) = pairs.next() else {
        return Ok(ExecutionPlan::default());
    };

    report_unparsed_tail(&input_cow, pair.as_span().end());

    build_commands(&mut ctx, pair)
}

fn report_unparsed_tail(input: &str, consumed: usize) {
    if let Some(tail) = parser::unparsed_tail(input, consumed) {
        tracing::warn!("unparsed input tail: {:?}", tail);
        eprint!("dsh: warning: ignored unparsed input: {tail}\r\n");
    }
}

/// Split one `NAME=value` into its two halves (pure).
fn parse_assignment(pair: Pair<Rule>) -> (String, String) {
    let mut name = String::new();
    let mut value = String::new();
    for part in pair.into_inner() {
        match part.as_rule() {
            Rule::assign_name => name = part.as_str().to_string(),
            Rule::span => value = parser::get_string(part).unwrap_or_default(),
            _ => {}
        }
    }
    (name, value)
}

/// Whether any part of this span is a substitution.
fn span_has_substitution(span: &Pair<Rule>) -> bool {
    span.clone().into_inner().any(|part| {
        matches!(
            part.as_rule(),
            Rule::subshell | Rule::proc_subst | Rule::command_subst
        )
    })
}

/// Build the redirections one `redirect` pair stands for (pure).
fn parse_redirect(pair: Pair<Rule>) -> Result<Vec<Redirect>> {
    let mut direction = None;

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::fd_dup => return parse_fd_dup(inner),
            Rule::stdout_redirect_direction
            | Rule::stderr_redirect_direction
            | Rule::stdouterr_redirect_direction
            | Rule::stdin_redirect_direction => {
                direction = inner.into_inner().next().map(|rule| rule.as_rule());
            }
            Rule::span => {
                let dest = parser::get_string(inner).unwrap_or_default();
                return Ok(match direction {
                    Some(Rule::stdout_redirect_direction_out) => {
                        vec![Redirect::write(STDOUT_FILENO, dest)]
                    }
                    Some(Rule::stdout_redirect_direction_append) => {
                        vec![Redirect::append(STDOUT_FILENO, dest)]
                    }
                    Some(Rule::stderr_redirect_direction_out) => {
                        vec![Redirect::write(STDERR_FILENO, dest)]
                    }
                    Some(Rule::stderr_redirect_direction_append) => {
                        vec![Redirect::append(STDERR_FILENO, dest)]
                    }
                    Some(Rule::stdouterr_redirect_direction_out) => Redirect::both(dest, false),
                    Some(Rule::stdouterr_redirect_direction_append) => Redirect::both(dest, true),
                    Some(Rule::stdin_redirect_direction_in) => vec![Redirect::input(dest)],
                    _ => Vec::new(),
                });
            }
            _ => {}
        }
    }

    Ok(Vec::new())
}

/// `2>&1`, `>&2`, `2>&-`.
fn parse_fd_dup(pair: Pair<Rule>) -> Result<Vec<Redirect>> {
    let Some(form) = pair.into_inner().next() else {
        return Ok(Vec::new());
    };
    let default_fd = match form.as_rule() {
        Rule::fd_dup_in => STDIN_FILENO,
        _ => STDOUT_FILENO,
    };

    let mut fd = default_fd;
    let mut target = None;
    for inner in form.into_inner() {
        match inner.as_rule() {
            Rule::fd_number => fd = parse_fd(inner.as_str())?,
            Rule::fd_dup_target => target = Some(inner.as_str().to_string()),
            _ => {}
        }
    }

    let Some(target) = target else {
        return Ok(Vec::new());
    };
    Ok(vec![if target == "-" {
        Redirect::close(fd)
    } else {
        Redirect::dup(fd, parse_fd(&target)?)
    }])
}

fn parse_fd(text: &str) -> Result<RawFd> {
    text.parse::<RawFd>()
        .with_context(|| format!("dsh: invalid file descriptor '{text}'"))
}

fn empty_command() -> PlannedCommand {
    PlannedCommand {
        argv: Vec::new(),
        redirects: Vec::new(),
        env_overrides: Vec::new(),
    }
}

fn empty_job(source: String, ctx: &ParseContext) -> PlannedJob {
    let subshell = if ctx.subshell {
        SubshellType::Subshell
    } else if ctx.proc_subst {
        SubshellType::ProcessSubstitution
    } else {
        SubshellType::None
    };
    PlannedJob {
        source,
        stages: Vec::new(),
        list_op: ListOp::None,
        foreground: ctx.foreground,
        capture_output: false,
        struct_pipe_exprs: Vec::new(),
        subshell,
    }
}

/// Collect one `simple_command` into a pipeline stage (pure, no execution).
fn build_simple_command(ctx: &ParseContext, pair: Pair<Rule>) -> Result<PlannedCommand> {
    let mut stage = empty_command();
    build_argv(ctx, &mut stage, pair)?;
    Ok(stage)
}

fn push_substitution(
    argv: &mut Vec<PlannedArg>,
    kind: SubshellType,
    inner_pair: Pair<Rule>,
    ctx: &ParseContext,
) -> Result<()> {
    let cmd_str = inner_pair.as_str().to_string();
    let mut nested = ParseContext::new(ctx.foreground);
    match kind {
        SubshellType::Subshell | SubshellType::CommandSubstitution => nested.subshell = true,
        SubshellType::ProcessSubstitution => nested.proc_subst = true,
        SubshellType::None => {}
    }
    let plan = build_commands(&mut nested, inner_pair)?;
    if plan.is_empty() {
        return Ok(());
    }
    argv.push(PlannedArg::Substitution(PlannedSubstitution {
        source: cmd_str,
        kind,
        plan: Box::new(plan),
    }));
    Ok(())
}

fn push_span_parts(argv: &mut Vec<PlannedArg>, span: Pair<Rule>, ctx: &ParseContext) -> Result<()> {
    if !span_has_substitution(&span) {
        if let Some(arg) = parser::get_string(span) {
            argv.push(PlannedArg::Literal(arg));
        }
        return Ok(());
    }
    for part in span.into_inner() {
        match part.as_rule() {
            Rule::subshell => {
                for inner in part.into_inner() {
                    push_substitution(argv, SubshellType::Subshell, inner, ctx)?;
                }
            }
            Rule::proc_subst => {
                for inner in part.into_inner() {
                    if inner.as_rule() == Rule::proc_subst_direction {
                        continue;
                    }
                    push_substitution(argv, SubshellType::ProcessSubstitution, inner, ctx)?;
                }
            }
            Rule::command_subst => {
                for inner in part.into_inner() {
                    push_substitution(argv, SubshellType::CommandSubstitution, inner, ctx)?;
                }
            }
            _ => {
                if let Some(arg) = parser::get_string(part) {
                    argv.push(PlannedArg::Literal(arg));
                }
            }
        }
    }
    Ok(())
}

fn build_argv(ctx: &ParseContext, stage: &mut PlannedCommand, pair: Pair<Rule>) -> Result<()> {
    for inner_pair in pair.into_inner() {
        match inner_pair.as_rule() {
            Rule::argv0 => {
                for span in inner_pair.into_inner() {
                    push_span_parts(&mut stage.argv, span, ctx)?;
                }
            }
            Rule::assignment_list => {
                for assignment in inner_pair.into_inner() {
                    stage.env_overrides.push(parse_assignment(assignment));
                }
            }
            Rule::args => {
                for item in inner_pair.into_inner() {
                    if let Rule::redirect = item.as_rule() {
                        stage.redirects.extend(parse_redirect(item)?);
                        continue;
                    }
                    push_span_parts(&mut stage.argv, item, ctx)?;
                }
            }
            Rule::simple_command => {
                let mut nested = empty_command();
                build_argv(ctx, &mut nested, inner_pair)?;
                stage.argv.extend(nested.argv);
                stage.redirects.extend(nested.redirects);
                stage.env_overrides.extend(nested.env_overrides);
            }
            _ => {
                warn!("missing {:?}", inner_pair.as_rule());
            }
        }
    }
    Ok(())
}

fn build_commands(ctx: &mut ParseContext, pair: Pair<Rule>) -> Result<ExecutionPlan> {
    let mut plan = ExecutionPlan::default();
    if let Rule::commands = pair.as_rule() {
        for pair in pair.into_inner() {
            match pair.as_rule() {
                Rule::command => build_jobs(ctx, pair, &mut plan.jobs)?,
                Rule::command_list_sep => {
                    if let Some(sep) = pair.into_inner().next()
                        && let Some(last) = plan.jobs.last_mut()
                    {
                        debug!("last job {:?}", &last.source);
                        match sep.as_rule() {
                            Rule::and_op => last.list_op = ListOp::And,
                            Rule::or_op => last.list_op = ListOp::Or,
                            _ => {}
                        }
                    }
                }
                _ => {
                    debug!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                }
            }
        }
    }
    debug!("planned jobs len: {}", plan.jobs.len());
    Ok(plan)
}

fn mark_nested_job(job: &mut PlannedJob, ctx: &ParseContext) {
    if ctx.subshell {
        job.subshell = SubshellType::Subshell;
    }
    if ctx.proc_subst {
        job.subshell = SubshellType::ProcessSubstitution;
    }
}

fn build_jobs(ctx: &mut ParseContext, pair: Pair<Rule>, jobs: &mut Vec<PlannedJob>) -> Result<()> {
    let job_str = pair.as_str().to_string();

    for inner_pair in pair.into_inner() {
        debug!(
            "find {:?}:'{:?}'",
            inner_pair.as_rule(),
            inner_pair.as_str()
        );
        match inner_pair.as_rule() {
            Rule::simple_command => {
                let mut job = empty_job(job_str.clone(), ctx);
                let stage = build_simple_command(ctx, inner_pair)?;
                job.stages.push(stage);
                mark_nested_job(&mut job, ctx);
                if !job.stages.iter().all(|stage| stage.is_empty()) {
                    jobs.push(job);
                }
            }
            Rule::simple_command_bg => {
                let mut job = empty_job(inner_pair.as_str().to_string(), ctx);
                job.foreground = false;
                for bg_pair in inner_pair.into_inner() {
                    if let Rule::simple_command = bg_pair.as_rule() {
                        let stage = build_simple_command(ctx, bg_pair)?;
                        job.stages.push(stage);
                        mark_nested_job(&mut job, ctx);
                        job.foreground = false;
                        if !job.stages.iter().all(|stage| stage.is_empty()) {
                            jobs.push(job);
                        }
                        break;
                    }
                }
            }
            Rule::pipe_command => {
                if jobs.is_empty() {
                    let mut job = empty_job(job_str.clone(), ctx);
                    mark_nested_job(&mut job, ctx);
                    jobs.push(job);
                }
                if let Some(job) = jobs.last_mut() {
                    let saved_foreground = ctx.foreground;
                    for stage_pair in inner_pair.into_inner() {
                        if let Rule::simple_command = stage_pair.as_rule() {
                            ctx.foreground = true;
                            let stage = build_simple_command(ctx, stage_pair)?;
                            job.stages.push(stage);
                        } else if let Rule::simple_command_bg = stage_pair.as_rule() {
                            ctx.foreground = false;
                            for bg_pair in stage_pair.into_inner() {
                                if let Rule::simple_command = bg_pair.as_rule() {
                                    let stage = build_simple_command(ctx, bg_pair)?;
                                    job.stages.push(stage);
                                    job.foreground = false;
                                    break;
                                }
                            }
                        }
                    }
                    ctx.foreground = saved_foreground;
                }
            }
            Rule::capture_suffix => {
                if let Some(job) = jobs.last_mut() {
                    job.capture_output = true;
                }
            }
            Rule::struct_pipe_command => {
                for expr_pair in inner_pair.into_inner() {
                    let lisp_expr = match expr_pair.as_rule() {
                        Rule::lisp_expr => expr_pair.as_str().to_string(),
                        Rule::struct_pipe_dsl => {
                            struct_pipe::desugar_or_error_call(expr_pair.as_str())
                        }
                        _ => continue,
                    };
                    if let Some(job) = jobs.last_mut() {
                        job.struct_pipe_exprs.push(lisp_expr);
                    } else {
                        let mut job = empty_job(job_str.clone(), ctx);
                        mark_nested_job(&mut job, ctx);
                        job.struct_pipe_exprs.push(lisp_expr);
                        jobs.push(job);
                    }
                }
            }
            _ => {
                warn!(
                    "missing rule {:?} {:?}",
                    inner_pair.as_rule(),
                    inner_pair.as_str()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;

    fn test_env() -> Arc<RwLock<Environment>> {
        Environment::new()
    }

    /// Test A: planning alone must not execute substitutions.
    #[test]
    fn planning_does_not_execute_substitution() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("parse_must_not_run");
        let input = format!("echo $(touch {})", marker.display());
        let env = test_env();
        let cwd = std::env::current_dir().expect("cwd");
        let vars_before = {
            let guard = env.read();
            guard.variable_state.variables.clone()
        };
        let plan = parse_execution_plan(&input, Arc::clone(&env)).expect("plan");
        assert_eq!(plan.jobs.len(), 1);
        assert!(plan.jobs[0].contains_deferred_evaluation());
        assert!(
            !marker.exists(),
            "planning executed a substitution it must only record"
        );
        assert_eq!(
            std::env::current_dir().expect("cwd"),
            cwd,
            "planning must not change directories"
        );
        let vars_after = env.read().variable_state.variables.clone();
        assert_eq!(
            vars_before, vars_after,
            "planning must not mutate variables"
        );
    }

    /// Test B: a standalone assignment is deferred, not applied by planning.
    #[test]
    fn planning_does_not_apply_standalone_assignment() {
        let env = test_env();
        let plan =
            parse_execution_plan("DOGESH_TEST_PARSE_ONLY=value", Arc::clone(&env)).expect("plan");
        assert_eq!(plan.jobs.len(), 1);
        assert!(env.read().get_var("DOGESH_TEST_PARSE_ONLY").is_none());
        assert!(plan.jobs[0].is_assignment_only());
    }
}
