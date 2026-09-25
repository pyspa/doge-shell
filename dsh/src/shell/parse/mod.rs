//! Side-effect-free shell planning: pest pairs become an `ExecutionPlan`.
//!
//! The parser may read aliases for syntax rewriting, but it does not resolve
//! runtime variables, tilde, glob/brace patterns, or substitutions. One source
//! span becomes one [`PlannedWord`]; runtime values are left for the selected
//! job's materialization.
//!
//! Execution planning is strict: any non-whitespace unparsed tail is a syntax
//! error and no prefix is executed. `Rule::commands` remains intentionally
//! tolerant for REPL highlighting and completion; the strictness lives only in
//! this execution path, which validates raw input before alias rewriting and
//! validates alias-rewritten input again.

use super::plan::{
    ExecutionPlan, ListExecutionMode, PlannedAndOrList, PlannedAssignment, PlannedCommand,
    PlannedJob, PlannedLiteral, PlannedRedirect, PlannedRedirectOp, PlannedSubstitution,
    PlannedSubstitutionKind, PlannedWord, ProcessSubstitutionDirection, QuoteMode, WordPart,
};
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

/// Pure parse context: nesting flags only, no shell handle.
///
/// Background state is not tracked here: `&` separates whole AND-OR lists
/// (see [`PlannedAndOrList`]), so nested substitutions never inherit a
/// background flag.
#[derive(Debug, Default)]
pub struct ParseContext {
    pub subshell: bool,
    pub proc_subst: bool,
}

impl ParseContext {
    pub fn new() -> Self {
        Self {
            subshell: false,
            proc_subst: false,
        }
    }
}

/// Parse `input` into a side-effect-free plan.
///
/// Only aliases are read (for syntax rewriting). No command is executed, no
/// variable or pattern is resolved, and no shell state is mutated.
///
/// Strict execution: the raw input is fully validated before rewriting (so an
/// unparsed suffix cannot be discarded), and a rewritten line is validated
/// again. Any non-whitespace leftover is a syntax error and no
/// `ExecutionPlan` is returned.
pub fn parse_execution_plan(
    input: &str,
    environment: Arc<RwLock<Environment>>,
) -> Result<ExecutionPlan> {
    // 1. Validate exactly what the user supplied BEFORE rewriting can discard
    // an unparsed suffix.
    validate_complete_commands(input)?;

    // 2. Syntax-time alias rewrite only; runtime expansion happens later.
    let aliased = parser::rewrite_aliases(input, environment)?;
    if let std::borrow::Cow::Owned(_) = aliased {
        validate_complete_commands(aliased.as_ref())?;
    }

    // 3. Parse the (possibly rewritten) line strictly.
    if aliased.trim().is_empty() {
        return Ok(ExecutionPlan::default());
    }
    let mut pairs = parse_commands_strict(aliased.as_ref())?;

    let mut ctx = ParseContext::new();
    let Some(pair) = pairs.next() else {
        return Ok(ExecutionPlan::default());
    };

    build_commands(&mut ctx, pair)
}

/// Parse `Rule::commands` and fail when any non-whitespace input is left over.
///
/// `Rule::commands` itself stays tolerant (REPL highlighting and completion
/// rely on partial parses); only the execution path uses this helper.
fn parse_commands_strict(input: &str) -> Result<pest::iterators::Pairs<'_, Rule>> {
    let pairs = ShellParser::parse(Rule::commands, input)
        .map_err(|e| anyhow::anyhow!("syntax error: {e}"))?;

    let consumed = pairs
        .clone()
        .next()
        .map(|pair| pair.as_span().end())
        .unwrap_or(0);

    if let Some(tail) = parser::unparsed_tail(input, consumed) {
        anyhow::bail!("syntax error: unexpected input {tail:?}");
    }

    Ok(pairs)
}

fn validate_complete_commands(input: &str) -> Result<()> {
    parse_commands_strict(input).map(|_| ())
}

/// Parse one `NAME=value` into a planned assignment (pure, no resolution).
fn parse_assignment(pair: Pair<Rule>, ctx: &ParseContext) -> Result<(String, PlannedWord)> {
    let mut name = String::new();
    let mut value: Option<PlannedWord> = None;
    for part in pair.into_inner() {
        match part.as_rule() {
            Rule::assign_name => name = part.as_str().to_string(),
            Rule::span => value = Some(parse_word(part, ctx)?),
            _ => {}
        }
    }
    Ok((name, value.unwrap_or_else(PlannedWord::empty)))
}

/// Build the redirections one `redirect` pair stands for (pure, no resolution).
fn parse_redirect(pair: Pair<Rule>, ctx: &ParseContext) -> Result<Vec<PlannedRedirect>> {
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
                let target = parse_word(inner, ctx)?;
                return Ok(match direction {
                    Some(Rule::stdout_redirect_direction_out) => {
                        vec![PlannedRedirect {
                            fd: STDOUT_FILENO,
                            op: PlannedRedirectOp::WriteFile(target),
                        }]
                    }
                    Some(Rule::stdout_redirect_direction_append) => {
                        vec![PlannedRedirect {
                            fd: STDOUT_FILENO,
                            op: PlannedRedirectOp::AppendFile(target),
                        }]
                    }
                    Some(Rule::stderr_redirect_direction_out) => {
                        vec![PlannedRedirect {
                            fd: STDERR_FILENO,
                            op: PlannedRedirectOp::WriteFile(target),
                        }]
                    }
                    Some(Rule::stderr_redirect_direction_append) => {
                        vec![PlannedRedirect {
                            fd: STDERR_FILENO,
                            op: PlannedRedirectOp::AppendFile(target),
                        }]
                    }
                    Some(Rule::stdouterr_redirect_direction_out) => vec![PlannedRedirect {
                        fd: STDOUT_FILENO,
                        op: PlannedRedirectOp::BothWrite(target),
                    }],
                    Some(Rule::stdouterr_redirect_direction_append) => vec![PlannedRedirect {
                        fd: STDOUT_FILENO,
                        op: PlannedRedirectOp::BothAppend(target),
                    }],
                    Some(Rule::stdin_redirect_direction_in) => vec![PlannedRedirect {
                        fd: STDIN_FILENO,
                        op: PlannedRedirectOp::ReadFile(target),
                    }],
                    _ => Vec::new(),
                });
            }
            _ => {}
        }
    }

    Ok(Vec::new())
}

/// `2>&1`, `>&2`, `2>&-`.
fn parse_fd_dup(pair: Pair<Rule>) -> Result<Vec<PlannedRedirect>> {
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
        PlannedRedirect {
            fd,
            op: PlannedRedirectOp::Close,
        }
    } else {
        PlannedRedirect {
            fd,
            op: PlannedRedirectOp::DupFrom(parse_fd(&target)?),
        }
    }])
}

fn parse_fd(text: &str) -> Result<RawFd> {
    text.parse::<RawFd>()
        .with_context(|| format!("dsh: invalid file descriptor '{text}'"))
}

/// Convert a concrete planned redirect back for dry assembly.
pub(crate) fn planned_to_concrete(redirect: &PlannedRedirect, target: String) -> Vec<Redirect> {
    match &redirect.op {
        PlannedRedirectOp::DupFrom(from) => vec![Redirect::dup(redirect.fd, *from)],
        PlannedRedirectOp::Close => vec![Redirect::close(redirect.fd)],
        _ => redirect.to_concrete(target),
    }
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
        capture_output: false,
        struct_pipe_exprs: Vec::new(),
        subshell,
        pipeline_source: None,
    }
}

/// Collect one `simple_command` into a pipeline stage (pure, no execution).
fn build_simple_command(ctx: &ParseContext, pair: Pair<Rule>) -> Result<PlannedCommand> {
    let mut stage = empty_command();
    build_argv(ctx, &mut stage, pair)?;
    Ok(stage)
}

fn make_substitution(
    kind: PlannedSubstitutionKind,
    commands_pair: Pair<Rule>,
    _ctx: &ParseContext,
) -> Result<Option<PlannedSubstitution>> {
    let cmd_str = commands_pair.as_str().to_string();
    let mut nested = ParseContext::new();
    match kind {
        PlannedSubstitutionKind::Command | PlannedSubstitutionKind::Subshell => {
            nested.subshell = true;
        }
        PlannedSubstitutionKind::Process(_) => nested.proc_subst = true,
    }
    let plan = build_commands(&mut nested, commands_pair)?;
    if plan.is_empty() {
        return Ok(None);
    }
    Ok(Some(PlannedSubstitution {
        source: cmd_str,
        kind,
        plan: Box::new(plan),
    }))
}

/// Grammar is the authority for `<(` vs `>(...)`: never sniff the string
/// prefix. Unknown rules fail closed.
fn parse_process_substitution_direction(pair: Pair<Rule>) -> Result<ProcessSubstitutionDirection> {
    match pair.as_rule() {
        Rule::proc_subst_direction_in => Ok(ProcessSubstitutionDirection::Read),
        Rule::proc_subst_direction_out => Ok(ProcessSubstitutionDirection::Write),
        Rule::proc_subst_direction => {
            // `proc_subst_direction` wraps one of the two concrete arms.
            let Some(inner) = pair.into_inner().next() else {
                anyhow::bail!("process substitution is missing a direction");
            };
            parse_process_substitution_direction(inner)
        }
        other => anyhow::bail!("unknown process substitution direction: {other:?}"),
    }
}

fn substitution_from_wrapper(
    wrapper: Pair<Rule>,
    kind: PlannedSubstitutionKind,
    ctx: &ParseContext,
) -> Result<Vec<PlannedSubstitution>> {
    let mut out = Vec::new();
    // `proc_subst` carries its direction inline; the caller passes a
    // placeholder that is replaced once the direction token is seen.
    let is_process = matches!(kind, PlannedSubstitutionKind::Process(_));
    let mut process_kind: Option<PlannedSubstitutionKind> =
        if is_process { Some(kind.clone()) } else { None };
    // Non-proc-subst wrappers (`$(...)`, `(...)`) keep `kind` as-is.
    let fixed_kind: Option<PlannedSubstitutionKind> = if is_process { None } else { Some(kind) };
    for inner in wrapper.into_inner() {
        match inner.as_rule() {
            Rule::proc_subst_direction => {
                let direction = parse_process_substitution_direction(inner)?;
                process_kind = Some(PlannedSubstitutionKind::Process(direction));
            }
            _ => {
                let active = match (&fixed_kind, &process_kind) {
                    (Some(fixed), _) => fixed.clone(),
                    (None, Some(process)) => process.clone(),
                    (None, None) => {
                        anyhow::bail!("process substitution is missing a direction");
                    }
                };
                if let Some(subst) = make_substitution(active, inner, ctx)? {
                    out.push(subst);
                }
            }
        }
    }
    Ok(out)
}

fn unquoted_literal(part: Pair<Rule>, first: bool) -> PlannedLiteral {
    let raw = part.as_str().to_string();
    let text = parser::get_string(part).unwrap_or_default();
    PlannedLiteral {
        text,
        raw: raw.clone(),
        quote: QuoteMode::Unquoted,
        pattern_active: false,
        brace_active: false,
        tilde_candidate: first && raw.starts_with('~'),
    }
}

fn active_literal(part: Pair<Rule>, first: bool) -> PlannedLiteral {
    let raw = part.as_str().to_string();
    let text = parser::get_string(part).unwrap_or_default();
    PlannedLiteral {
        text,
        raw: raw.clone(),
        quote: QuoteMode::Unquoted,
        pattern_active: true,
        brace_active: true,
        tilde_candidate: first && raw.starts_with('~'),
    }
}

/// One `span` becomes one [`PlannedWord`]; runtime values are never resolved.
fn parse_word(span: Pair<Rule>, ctx: &ParseContext) -> Result<PlannedWord> {
    let source = span.as_str().to_string();
    let mut parts = Vec::new();
    let mut first = true;
    for part in span.into_inner() {
        match part.as_rule() {
            Rule::word => parts.push(WordPart::Literal(unquoted_literal(part, first))),
            Rule::glob_word | Rule::brace_word => {
                parts.push(WordPart::Literal(active_literal(part, first)));
            }
            Rule::variable => parts.push(WordPart::Variable {
                source: part.as_str().to_string(),
                quote: QuoteMode::Unquoted,
            }),
            Rule::s_quoted => {
                let raw = part.as_str().to_string();
                let text = parser::get_string(part).unwrap_or_default();
                parts.push(WordPart::Literal(PlannedLiteral {
                    text,
                    raw,
                    quote: QuoteMode::Single,
                    pattern_active: false,
                    brace_active: false,
                    tilde_candidate: false,
                }));
            }
            Rule::d_quoted => {
                for inner in part.into_inner() {
                    match inner.as_rule() {
                        Rule::variable => parts.push(WordPart::Variable {
                            source: inner.as_str().to_string(),
                            quote: QuoteMode::Double,
                        }),
                        Rule::command_subst => {
                            for subst in substitution_from_wrapper(
                                inner,
                                PlannedSubstitutionKind::Command,
                                ctx,
                            )? {
                                parts.push(WordPart::Substitution {
                                    substitution: subst,
                                    quote: QuoteMode::Double,
                                });
                            }
                        }
                        _ => {
                            let raw = inner.as_str().to_string();
                            let text = parser::get_string(inner).unwrap_or_default();
                            parts.push(WordPart::Literal(PlannedLiteral {
                                text,
                                raw,
                                quote: QuoteMode::Double,
                                pattern_active: false,
                                brace_active: false,
                                tilde_candidate: false,
                            }));
                        }
                    }
                }
            }
            Rule::command_subst => {
                for subst in substitution_from_wrapper(part, PlannedSubstitutionKind::Command, ctx)?
                {
                    parts.push(WordPart::Substitution {
                        substitution: subst,
                        quote: QuoteMode::Unquoted,
                    });
                }
            }
            Rule::proc_subst => {
                // Direction comes from the inline `proc_subst_direction`
                // token; the placeholder is replaced once it is seen.
                for subst in substitution_from_wrapper(
                    part,
                    PlannedSubstitutionKind::Process(ProcessSubstitutionDirection::Read),
                    ctx,
                )? {
                    parts.push(WordPart::Substitution {
                        substitution: subst,
                        quote: QuoteMode::Unquoted,
                    });
                }
            }
            Rule::subshell => {
                for subst in
                    substitution_from_wrapper(part, PlannedSubstitutionKind::Subshell, ctx)?
                {
                    parts.push(WordPart::Substitution {
                        substitution: subst,
                        quote: QuoteMode::Unquoted,
                    });
                }
            }
            _ => {
                let raw = part.as_str().to_string();
                if let Some(text) = parser::get_string(part) {
                    parts.push(WordPart::Literal(PlannedLiteral {
                        raw,
                        text,
                        quote: QuoteMode::Unquoted,
                        pattern_active: false,
                        brace_active: false,
                        tilde_candidate: false,
                    }));
                }
            }
        }
        first = false;
    }
    Ok(PlannedWord { source, parts })
}

fn build_argv(ctx: &ParseContext, stage: &mut PlannedCommand, pair: Pair<Rule>) -> Result<()> {
    for inner_pair in pair.into_inner() {
        match inner_pair.as_rule() {
            Rule::argv0 => {
                for span in inner_pair.into_inner() {
                    stage.argv.push(parse_word(span, ctx)?);
                }
            }
            Rule::assignment => {
                let (name, value) = parse_assignment(inner_pair, ctx)?;
                stage.env_overrides.push(PlannedAssignment { name, value });
            }
            Rule::redirect => {
                stage.redirects.extend(parse_redirect(inner_pair, ctx)?);
            }
            Rule::args => {
                for item in inner_pair.into_inner() {
                    if let Rule::redirect = item.as_rule() {
                        stage.redirects.extend(parse_redirect(item, ctx)?);
                        continue;
                    }
                    stage.argv.push(parse_word(item, ctx)?);
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
                Rule::and_or_list => {
                    let mut list = PlannedAndOrList {
                        source: pair.as_str().trim().to_string(),
                        jobs: Vec::new(),
                        execution: ListExecutionMode::Foreground,
                    };
                    build_and_or_list(ctx, pair, &mut list)?;
                    if !list.jobs.is_empty() {
                        plan.lists.push(list);
                    }
                }
                Rule::list_separator => {
                    // Both inter-list separators (`a & b`, `a; b`) and the
                    // trailing terminator (`sleep 1 &`, `echo hi;`) arrive
                    // here. A separator always follows a list: the grammar
                    // cannot start `commands` with one.
                    let Some(last) = plan.lists.last_mut() else {
                        anyhow::bail!("syntax error: unexpected list separator");
                    };
                    if let Some(sep) = pair.into_inner().next() {
                        debug!("last list {:?} sep {:?}", &last.source, sep.as_rule());
                        if sep.as_rule() == Rule::background_op {
                            last.execution = ListExecutionMode::Asynchronous;
                        }
                    }
                }
                _ => {
                    debug!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                }
            }
        }
    }
    debug!("planned lists len: {}", plan.lists.len());
    Ok(plan)
}

/// Build one AND-OR list: `&&`/`||`-gated pipelines sharing one execution
/// environment. The gate resets per list — a `;`/`&` boundary never carries
/// the previous list's `ListOp` forward.
fn build_and_or_list(
    ctx: &mut ParseContext,
    pair: Pair<Rule>,
    list: &mut PlannedAndOrList,
) -> Result<()> {
    if let Rule::and_or_list = pair.as_rule() {
        for pair in pair.into_inner() {
            match pair.as_rule() {
                Rule::command => build_jobs(ctx, pair, &mut list.jobs)?,
                Rule::and_or_op => {
                    if let Some(sep) = pair.into_inner().next()
                        && let Some(last) = list.jobs.last_mut()
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
    Ok(())
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
    // Pest spans may carry surrounding whitespace (e.g. the space before a
    // `&` separator); user-facing sources stay trimmed.
    let job_str = pair.as_str().trim().to_string();

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
            Rule::pipe_command => {
                if jobs.is_empty() {
                    let mut job = empty_job(job_str.clone(), ctx);
                    mark_nested_job(&mut job, ctx);
                    jobs.push(job);
                }
                if let Some(job) = jobs.last_mut() {
                    for stage_pair in inner_pair.into_inner() {
                        if let Rule::simple_command = stage_pair.as_rule() {
                            let stage = build_simple_command(ctx, stage_pair)?;
                            job.stages.push(stage);
                        }
                    }
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
mod tests;
