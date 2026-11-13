//! Runtime word expansion for selected jobs.
//!
//! Planning preserves one source word as structured parts. This module reads
//! current shell state at materialization time (variables, `$?`, `HOME`, cwd)
//! and builds concrete fields. Alias rewriting is syntax-time only and never
//! happens here.
//!
//! Field splitting is IFS-aware and runs across the whole expanded word via
//! `super::field_split`: only unquoted parameter and command-substitution
//! results may delimit, and splitting happens before pathname expansion.
//!
//! Structured `${VAR-op}` expansions splice provenance-bearing segments into
//! the same outer stream; the operand is expanded lazily only when the state
//! matrix selects it.

mod dry;
mod redirect;
mod segments;

pub use dry::{dry_expand_argument_word, dry_expand_scalar_word};
pub(crate) use redirect::AmbiguousRedirectError;
pub use redirect::expand_redirect_target;
pub(crate) use segments::{
    ExpansionContext, dynamic_text_segment, literal_segment, resolve_parameter, variable_segment,
};
use segments::{finish_split_fields, resolve_ifs};

use super::authorize::ConfirmFn;
use super::field_split::{ExpandedSegment, PatternKind, split_segments};
use super::plan::{PlannedSubstitutionKind, PlannedWord, QuoteMode, WordPart};
use super::process_substitution::{ExecutionResources, start_process_substitution};
use super::substitution::capture_subshell_plan_stdout;
use super::word_expand_invocation::{invocation_scalar, invocation_segments};
use crate::parser::expansion::escape_glob_metacharacters;
use crate::process::reexec::PlanExecMode;
use crate::shell::expansion_host::ExpansionHost;
use anyhow::Result;
use dsh_types::Context;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

fn cwd_for_expansion() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub(crate) fn trim_substitution_output(output: &str) -> String {
    output.trim_end_matches('\n').to_string()
}

/// Per-stage trace of runtime expansion, kept in stage order.
///
/// Command substitution (`$(...)`) and subshell groups (`( ... )`) update
/// `last_command_substitution_status`; process substitution (`<(...)` /
/// `>(...)`) never does. The value is consumed only when runtime expansion
/// leaves no command name (Bash `3.7.1 Simple Command Expansion`); a
/// surviving command reports its own execution status instead. That is how
/// `(false); echo $?` reports 1: the group prints nothing, so no command is
/// left and the group's status becomes the status.
#[derive(Debug, Default)]
pub struct ExpansionTrace {
    pub last_command_substitution_status: Option<i32>,
}

/// Build provenance-bearing segments for one word without splitting.
///
/// Shared by argument and parameter-operand expansion so `${...word...}`
/// splices into the same outer stream and whole-word splitting happens once
/// afterwards. Boxed because nested `${...}` makes this naturally recursive.
pub(crate) fn expand_word_segments<'a>(
    shell: &'a mut impl ExpansionHost,
    ctx: &'a Context,
    word: &'a PlannedWord,
    confirm: ConfirmFn,
    resources: &'a mut ExecutionResources,
    trace: &'a mut ExpansionTrace,
    context: ExpansionContext,
) -> Pin<Box<dyn Future<Output = Result<Vec<ExpandedSegment>>> + 'a>> {
    Box::pin(async move {
        let mut segments = Vec::with_capacity(word.parts.len());
        let mut first_part = true;
        for part in &word.parts {
            match part {
                WordPart::Literal(literal) => {
                    segments.push(literal_segment(literal, shell, first_part, context));
                }
                WordPart::InvocationParameter { parameter, quote } => {
                    segments.extend(invocation_segments(*parameter, *quote, shell, context));
                }
                WordPart::Variable { source, quote } => {
                    segments.push(variable_segment(source, *quote, shell, context));
                }
                WordPart::ParameterExpansion { expansion, quote } => {
                    let produced = super::word_expand_param::expand_parameter_to_segments(
                        shell, ctx, expansion, *quote, confirm, resources, trace,
                    )
                    .await?;
                    segments.extend(produced);
                }
                WordPart::ArithmeticExpansion { expansion, quote } => {
                    let produced = super::word_expand_arithmetic::expand_arithmetic_to_segments(
                        shell, ctx, expansion, *quote, confirm, resources, trace, context,
                    )
                    .await?;
                    segments.extend(produced);
                }
                WordPart::Substitution {
                    substitution,
                    quote,
                } => {
                    let quoted = *quote != QuoteMode::Unquoted;
                    match substitution.kind {
                        PlannedSubstitutionKind::Command | PlannedSubstitutionKind::Subshell => {
                            let mode = match substitution.kind {
                                PlannedSubstitutionKind::Subshell => PlanExecMode::Subshell,
                                _ => PlanExecMode::CommandSubstitution,
                            };
                            let captured = capture_subshell_plan_stdout(
                                shell,
                                ctx,
                                &substitution.plan,
                                mode,
                                confirm,
                            )
                            .await?;
                            trace.last_command_substitution_status = Some(captured.exit_code);
                            let value = trim_substitution_output(&captured.stdout);
                            segments.push(dynamic_text_segment(value, quoted, context));
                        }
                        PlannedSubstitutionKind::Process(direction) => {
                            let substitution = start_process_substitution(
                                shell,
                                ctx,
                                &substitution.plan,
                                direction,
                                confirm,
                            )
                            .await?;
                            let path = resources.add_process_substitution(substitution);
                            segments.push(ExpandedSegment::protected(
                                path.clone(),
                                escape_glob_metacharacters(&path),
                                PatternKind::Inactive,
                                true,
                            ));
                        }
                    }
                }
            }
            first_part = false;
        }
        Ok(segments)
    })
}

/// Expand one word into zero, one, or many argument fields.
pub async fn expand_argument_word(
    shell: &mut impl ExpansionHost,
    ctx: &Context,
    word: &PlannedWord,
    confirm: ConfirmFn,
    resources: &mut ExecutionResources,
    trace: &mut ExpansionTrace,
) -> Result<Vec<String>> {
    if word.parts.is_empty() {
        return Ok(Vec::new());
    }
    let ifs = resolve_ifs(shell);
    let context = ExpansionContext::Argument;
    let segments =
        expand_word_segments(shell, ctx, word, confirm, resources, trace, context).await?;
    debug_assert!(context.do_field_split());
    let split = split_segments(&segments, &ifs);
    let cwd = cwd_for_expansion();
    Ok(finish_split_fields(&split, &cwd))
}

/// Expand an assignment value into exactly one string: no splitting, no glob.
pub async fn expand_assignment_value(
    shell: &mut impl ExpansionHost,
    ctx: &Context,
    word: &PlannedWord,
    confirm: ConfirmFn,
    resources: &mut ExecutionResources,
    trace: &mut ExpansionTrace,
) -> Result<String> {
    expand_scalar_word(shell, ctx, word, confirm, resources, trace).await
}

async fn expand_scalar_word(
    shell: &mut impl ExpansionHost,
    ctx: &Context,
    word: &PlannedWord,
    confirm: ConfirmFn,
    resources: &mut ExecutionResources,
    trace: &mut ExpansionTrace,
) -> Result<String> {
    if word.parts.is_empty() {
        return Ok(String::new());
    }
    let context = ExpansionContext::Assignment;
    let mut out = String::new();
    let mut first_part = true;
    for part in &word.parts {
        match part {
            WordPart::Literal(literal) => {
                let seg = literal_segment(literal, shell, first_part, context);
                out.push_str(&seg.text);
            }
            WordPart::InvocationParameter { parameter, .. } => {
                out.push_str(&invocation_scalar(*parameter, shell));
            }
            WordPart::Variable { source, quote } => {
                let seg = variable_segment(source, *quote, shell, context);
                out.push_str(&seg.text);
            }
            WordPart::ParameterExpansion { expansion, quote } => {
                let scalar = super::word_expand_param::expand_parameter_to_scalar(
                    shell, ctx, expansion, *quote, confirm, resources, trace,
                )
                .await?;
                out.push_str(&scalar);
            }
            WordPart::ArithmeticExpansion { expansion, .. } => {
                let scalar = super::word_expand_arithmetic::expand_arithmetic_to_decimal(
                    shell, ctx, expansion, confirm, resources, trace,
                )
                .await?;
                out.push_str(&scalar);
            }
            WordPart::Substitution { substitution, .. } => match substitution.kind {
                PlannedSubstitutionKind::Command | PlannedSubstitutionKind::Subshell => {
                    let mode = match substitution.kind {
                        PlannedSubstitutionKind::Subshell => PlanExecMode::Subshell,
                        _ => PlanExecMode::CommandSubstitution,
                    };
                    let captured =
                        capture_subshell_plan_stdout(shell, ctx, &substitution.plan, mode, confirm)
                            .await?;
                    trace.last_command_substitution_status = Some(captured.exit_code);
                    // Scalar context never splits; keep newlines except trailing.
                    out.push_str(&trim_substitution_output(&captured.stdout));
                }
                PlannedSubstitutionKind::Process(direction) => {
                    let substitution = start_process_substitution(
                        shell,
                        ctx,
                        &substitution.plan,
                        direction,
                        confirm,
                    )
                    .await?;
                    out.push_str(&resources.add_process_substitution(substitution));
                }
            },
        }
        first_part = false;
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
