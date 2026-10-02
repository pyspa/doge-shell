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

use super::authorize::ConfirmFn;
use super::field_split::{ExpandedSegment, IfsSpec, PatternKind, SplitField, split_segments};
use super::parameter_expand::ParameterState;
use super::plan::{PlannedLiteral, PlannedSubstitutionKind, PlannedWord, QuoteMode, WordPart};
use super::process_substitution::{ExecutionResources, start_process_substitution};
use super::substitution::capture_subshell_plan_stdout;
use crate::parser::expansion::{
    escape_brace_metacharacters, escape_glob_metacharacters, expand_braces, expand_glob_pattern,
    unescape_glob_metacharacters,
};
use crate::process::reexec::PlanExecMode;
use crate::shell::Shell;
use anyhow::{Result, bail};
use dsh_types::Context;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

/// Set/unset-preserving parameter resolution.
///
/// Ordinary `$FOO` maps both unset and set-empty to an empty value, but the
/// distinction is kept for `${FOO-default}`. Never fall back to the literal
/// source spelling for unset parameters.
pub(crate) fn resolve_parameter(source: &str, shell: &Shell) -> ParameterState {
    match shell.environment.read().get_var(source) {
        Some(value) => ParameterState::set(value),
        None => ParameterState::unset(),
    }
}

/// Which expansion semantics apply to one word.
///
/// Argument words field-split on IFS and allow unquoted dynamics to glob.
/// Assignment right-hand sides are scalar (no split, no glob). Redirect
/// targets are scalar for splitting (a variable containing spaces stays one
/// target) but keep source glob behaviour so existing ambiguous-wildcard
/// contracts hold. DryArgument mirrors Argument without executing bodies.
/// ParameterOperand is the selected `word` inside `${X:-word}`: unquoted
/// operand text is expansion-produced and splittable, escaped/quoted stays
/// protected, nested unquoted dynamics stay splittable with glob active and
/// braces inactive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExpansionContext {
    Argument,
    Assignment,
    Redirect,
    DryArgument,
    ParameterOperand,
}

impl ExpansionContext {
    fn do_field_split(self) -> bool {
        matches!(
            self,
            ExpansionContext::Argument
                | ExpansionContext::DryArgument
                | ExpansionContext::ParameterOperand
        )
    }

    /// Whether literal source patterns stay live for brace/glob matching.
    fn source_glob_active(self) -> bool {
        matches!(
            self,
            ExpansionContext::Argument | ExpansionContext::Redirect | ExpansionContext::DryArgument
        )
    }
}

fn lookup_home(shell: &Shell) -> Option<String> {
    shell.environment.read().lookup_variable("HOME")
}

fn apply_tilde(text: &str, pattern: &str, shell: &Shell) -> (String, String) {
    if !text.starts_with('~') {
        return (text.to_string(), pattern.to_string());
    }
    // A bare `~` reads the shell's current HOME so same-line assignments
    // are visible. Anything else starting with `~` (`~user/...`, or `~`
    // with no shell HOME, which is practically unreachable) falls through
    // to the historical system lookup below.
    if (text == "~" || text.starts_with("~/"))
        && let Some(home) = lookup_home(shell)
    {
        let rest = &text[1..];
        let rest_pattern = pattern.strip_prefix('~').unwrap_or(pattern);
        return (
            format!("{home}{rest}"),
            format!("{}{rest_pattern}", escape_glob_metacharacters(&home)),
        );
    }
    // `~user/...` was historically resolved through the system user database
    // by `shellexpand`; keep that behavior instead of going literal.
    let expanded = shellexpand::tilde(text).into_owned();
    if expanded == text {
        (text.to_string(), pattern.to_string())
    } else {
        (expanded.clone(), escape_glob_metacharacters(&expanded))
    }
}

fn resolve_ifs(shell: &Shell) -> IfsSpec {
    let value = shell.environment.read().lookup_variable("IFS");
    IfsSpec::resolve(value.as_deref())
}

pub(crate) fn literal_segment(
    literal: &PlannedLiteral,
    shell: &Shell,
    first: bool,
    ctx: ExpansionContext,
) -> ExpandedSegment {
    // Parameter operands never tilde-expand: `~` stays literal content.
    if ctx == ExpansionContext::ParameterOperand {
        match literal.quote {
            QuoteMode::Unquoted => {
                return ExpandedSegment::splittable_dynamic(
                    literal.text.clone(),
                    escape_brace_metacharacters(&literal.text),
                );
            }
            _ => {
                let pattern = escape_glob_metacharacters(&literal.text);
                let preserve = !literal.text.is_empty() || literal.quote != QuoteMode::Unquoted;
                return ExpandedSegment::protected(
                    literal.text.clone(),
                    pattern,
                    PatternKind::Inactive,
                    preserve,
                );
            }
        }
    }
    let (mut text, mut raw) = (literal.text.clone(), literal.raw.clone());
    if first && literal.tilde_candidate {
        (text, raw) = apply_tilde(&text, &raw, shell);
    }
    if !ctx.source_glob_active() {
        // Scalar assignment context: no brace/glob, pattern unused.
        return ExpandedSegment::protected(text, String::new(), PatternKind::Inactive, true);
    }
    match literal.quote {
        QuoteMode::Unquoted if literal.pattern_active || literal.brace_active => {
            ExpandedSegment::protected(
                text,
                raw,
                PatternKind::Source {
                    glob: literal.pattern_active,
                    brace: literal.brace_active,
                },
                false,
            )
        }
        _ => {
            let pattern = escape_glob_metacharacters(&text);
            let preserve = !text.is_empty() || literal.quote != QuoteMode::Unquoted;
            ExpandedSegment::protected(text, pattern, PatternKind::Inactive, preserve)
        }
    }
}

pub(crate) fn variable_segment(
    source: &str,
    quote: QuoteMode,
    shell: &Shell,
    ctx: ExpansionContext,
) -> ExpandedSegment {
    let resolved = resolve_parameter(source, shell);
    let quoted = quote != QuoteMode::Unquoted;
    match ctx {
        ExpansionContext::Assignment | ExpansionContext::Redirect => {
            // Scalar contexts: no splitting, no dynamic glob. Unquoted
            // empties still contribute `""` to the single scalar value.
            ExpandedSegment::protected(resolved.value, String::new(), PatternKind::Inactive, true)
        }
        ExpansionContext::Argument
        | ExpansionContext::DryArgument
        | ExpansionContext::ParameterOperand => {
            if quoted {
                ExpandedSegment::protected(
                    resolved.value.clone(),
                    escape_glob_metacharacters(&resolved.value),
                    PatternKind::Inactive,
                    true,
                )
            } else {
                // Unquoted: split-eligible, glob-active, brace-inactive.
                ExpandedSegment::splittable_dynamic(
                    resolved.value.clone(),
                    escape_brace_metacharacters(&resolved.value),
                )
            }
        }
    }
}

pub(crate) fn dynamic_text_segment(
    value: String,
    quoted: bool,
    ctx: ExpansionContext,
) -> ExpandedSegment {
    match ctx {
        ExpansionContext::Assignment | ExpansionContext::Redirect => {
            ExpandedSegment::protected(value, String::new(), PatternKind::Inactive, true)
        }
        // Argument/DryArgument/ParameterOperand: quoted stays protected;
        // unquoted is split-eligible with glob active and braces inactive.
        ExpansionContext::Argument
        | ExpansionContext::DryArgument
        | ExpansionContext::ParameterOperand => {
            if quoted {
                ExpandedSegment::protected(
                    value.clone(),
                    escape_glob_metacharacters(&value),
                    PatternKind::Inactive,
                    true,
                )
            } else {
                ExpandedSegment::splittable_dynamic(
                    value.clone(),
                    escape_brace_metacharacters(&value),
                )
            }
        }
    }
}

fn cwd_for_expansion() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub(crate) fn trim_substitution_output(output: &str) -> String {
    output.trim_end_matches('\n').to_string()
}

fn finish_split_fields(fields: &[SplitField], cwd: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for field in fields {
        if !field.has_active_pattern && !field.has_brace {
            if field.text.is_empty() && !field.preserve_empty {
                continue;
            }
            out.push(field.text.clone());
            continue;
        }
        for expanded in expand_braces(&field.pattern) {
            // `expand_glob_pattern` braces again harmlessly; unescape turns
            // a no-match pattern back into its literal view.
            for matched in expand_glob_pattern(&expanded, cwd) {
                out.push(unescape_glob_metacharacters(&matched));
            }
        }
    }
    out
}

/// Per-stage trace of runtime expansion, kept in stage order.
///
/// Only `PlannedSubstitutionKind::Command` updates
/// `last_command_substitution_status`. Process substitution (`<(...)` /
/// `>(...)`) and subshell groups (`( ... )`) have different semantics and
/// must never feed this status. The value is consumed only when runtime
/// expansion leaves no command name (Bash `3.7.1 Simple Command Expansion`);
/// a surviving command reports its own execution status instead.
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
    shell: &'a mut Shell,
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
                            if substitution.kind == PlannedSubstitutionKind::Command {
                                trace.last_command_substitution_status = Some(captured.exit_code);
                            }
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
    shell: &mut Shell,
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
    shell: &mut Shell,
    ctx: &Context,
    word: &PlannedWord,
    confirm: ConfirmFn,
    resources: &mut ExecutionResources,
    trace: &mut ExpansionTrace,
) -> Result<String> {
    expand_scalar_word(shell, ctx, word, confirm, resources, trace).await
}

/// Expand a redirect target into exactly one path.
///
/// Scalar for IFS splitting (a variable containing spaces stays one
/// target) while preserving source glob behaviour for ambiguous-wildcard
/// detection. Unquoted dynamics stay protected from both splitting and
/// pathname expansion, matching the pre-existing redirect contract.
pub async fn expand_redirect_target(
    shell: &mut Shell,
    ctx: &Context,
    word: &PlannedWord,
    confirm: ConfirmFn,
    resources: &mut ExecutionResources,
    trace: &mut ExpansionTrace,
) -> Result<String> {
    let context = ExpansionContext::Redirect;
    let mut text = String::new();
    let mut pattern = String::new();
    let mut has_active = false;
    let mut has_brace = false;
    let mut first_part = true;
    for part in &word.parts {
        match part {
            WordPart::Literal(literal) => {
                let seg = literal_segment(literal, shell, first_part, context);
                text.push_str(&seg.text);
                pattern.push_str(&seg.pattern);
                match seg.pattern_kind {
                    PatternKind::Inactive | PatternKind::DynamicGlob => {}
                    PatternKind::Source { glob, brace } => {
                        has_active |= glob;
                        has_brace |= brace;
                    }
                }
            }
            WordPart::Variable { source, quote } => {
                let seg = variable_segment(source, *quote, shell, context);
                text.push_str(&seg.text);
                // Redirect dynamics stay literal: `variable_segment` for
                // `Redirect` always returns an inactive segment with an
                // empty pattern, so escape the value for the literal pattern
                // (keeps `*` from globbing while preserving the spelling).
                // `seg.pattern_kind` is never `DynamicGlob` here.
                pattern.push_str(&escape_glob_metacharacters(&seg.text));
            }
            WordPart::ParameterExpansion { expansion, quote } => {
                // Scalar redirect contract: no IFS split, dynamics literal.
                let scalar = super::word_expand_param::expand_parameter_to_scalar(
                    shell, ctx, expansion, *quote, confirm, resources, trace,
                )
                .await?;
                text.push_str(&scalar);
                pattern.push_str(&escape_glob_metacharacters(&scalar));
            }
            WordPart::ArithmeticExpansion { expansion, .. } => {
                // Scalar redirect contract: decimal text, dynamics literal.
                let scalar = super::word_expand_arithmetic::expand_arithmetic_to_decimal(
                    shell, ctx, expansion, confirm, resources, trace,
                )
                .await?;
                text.push_str(&scalar);
                pattern.push_str(&escape_glob_metacharacters(&scalar));
            }
            WordPart::Substitution { substitution, .. } => {
                // Redirect targets are scalar: quoting affects neither
                // splitting (disabled) nor globbing (dynamics are literal),
                // so the quote mode is intentionally ignored here.
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
                        if substitution.kind == PlannedSubstitutionKind::Command {
                            trace.last_command_substitution_status = Some(captured.exit_code);
                        }
                        let value = trim_substitution_output(&captured.stdout);
                        text.push_str(&value);
                        pattern.push_str(&escape_glob_metacharacters(&value));
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
                        text.push_str(&path);
                        pattern.push_str(&escape_glob_metacharacters(&path));
                    }
                }
            }
        }
        first_part = false;
    }
    if word.parts.is_empty() {
        bail!("ambiguous redirect: '{}' expands to 0 fields", word.source);
    }
    // Single scalar field; run the existing brace/glob path so `*.txt`
    // matching several files still reports ambiguous.
    let field = SplitField {
        text,
        pattern,
        has_active_pattern: has_active,
        has_brace,
        preserve_empty: true,
    };
    let cwd = cwd_for_expansion();
    let fields = finish_split_fields(std::slice::from_ref(&field), &cwd);
    if fields.len() != 1 {
        bail!(
            "ambiguous redirect: '{}' expands to {} fields",
            word.source,
            fields.len()
        );
    }
    Ok(fields.into_iter().next().expect("one field"))
}

async fn expand_scalar_word(
    shell: &mut Shell,
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
                    if substitution.kind == PlannedSubstitutionKind::Command {
                        trace.last_command_substitution_status = Some(captured.exit_code);
                    }
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

/// Read-only expansion for safety preflight: no spawn, no env mutation.
///
/// Mirrors [`expand_argument_word`] with the same IFS semantics for current
/// variable values; substitution bodies stay diagnostic placeholders.
pub fn dry_expand_argument_word(
    word: &PlannedWord,
    shell: &Shell,
    cwd: &std::path::Path,
) -> Vec<String> {
    if word.parts.is_empty() {
        return Vec::new();
    }
    let ifs = resolve_ifs(shell);
    let context = ExpansionContext::DryArgument;
    let mut segments = Vec::with_capacity(word.parts.len());
    let mut first_part = true;
    for part in &word.parts {
        match part {
            WordPart::Literal(literal) => {
                segments.push(literal_segment(literal, shell, first_part, context));
            }
            WordPart::Variable { source, quote } => {
                segments.push(variable_segment(source, *quote, shell, context));
            }
            WordPart::ParameterExpansion { expansion, quote } => {
                segments.extend(super::word_expand_param::dry_parameter_to_segments(
                    expansion, *quote, shell,
                ));
            }
            WordPart::ArithmeticExpansion { expansion, quote } => {
                segments.extend(super::word_expand_arithmetic::dry_arithmetic_to_segments(
                    expansion, *quote, shell,
                ));
            }
            WordPart::Substitution {
                substitution,
                quote,
            } => {
                let placeholder = match substitution.kind {
                    PlannedSubstitutionKind::Command => format!("$({})", substitution.source),
                    PlannedSubstitutionKind::Process(
                        super::plan::ProcessSubstitutionDirection::Read,
                    ) => format!("<({})", substitution.source),
                    PlannedSubstitutionKind::Process(
                        super::plan::ProcessSubstitutionDirection::Write,
                    ) => format!(">({})", substitution.source),
                    PlannedSubstitutionKind::Subshell => format!("({})", substitution.source),
                };
                let quoted = *quote != QuoteMode::Unquoted;
                // Placeholders never split or glob; they only shape the dry
                // argv for SafetyGuard inspection.
                segments.push(ExpandedSegment::protected(
                    placeholder.clone(),
                    escape_glob_metacharacters(&placeholder),
                    PatternKind::Inactive,
                    quoted || !placeholder.is_empty(),
                ));
            }
        }
        first_part = false;
    }
    debug_assert!(context.do_field_split());
    finish_split_fields(&split_segments(&segments, &ifs), cwd)
}

/// Read-only scalar expansion for safety preflight.
pub fn dry_expand_scalar_word(word: &PlannedWord, shell: &Shell) -> String {
    let mut out = String::new();
    let mut first_part = true;
    for part in &word.parts {
        match part {
            WordPart::Literal(literal) => {
                let seg = literal_segment(literal, shell, first_part, ExpansionContext::Assignment);
                out.push_str(&seg.text);
            }
            WordPart::Variable { source, .. } => {
                out.push_str(&resolve_parameter(source, shell).value);
            }
            WordPart::ParameterExpansion { expansion, .. } => {
                out.push_str(&super::word_expand_param::dry_parameter_to_scalar(
                    expansion, shell,
                ));
            }
            WordPart::ArithmeticExpansion { expansion, .. } => {
                out.push_str(&super::word_expand_arithmetic::dry_arithmetic_to_scalar(
                    expansion, shell,
                ));
            }
            WordPart::Substitution { substitution, .. } => match substitution.kind {
                PlannedSubstitutionKind::Command => {
                    out.push_str(&format!("$({})", substitution.source));
                }
                PlannedSubstitutionKind::Process(
                    super::plan::ProcessSubstitutionDirection::Read,
                ) => {
                    out.push_str(&format!("<({})", substitution.source));
                }
                PlannedSubstitutionKind::Process(
                    super::plan::ProcessSubstitutionDirection::Write,
                ) => {
                    out.push_str(&format!(">({})", substitution.source));
                }
                PlannedSubstitutionKind::Subshell => {
                    out.push_str(&format!("({})", substitution.source));
                }
            },
        }
        first_part = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repl::confirmation::ConfirmationAction;
    use anyhow::Result;
    use std::sync::Arc;

    fn allow_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::Yes)
    }

    fn shell_with_empty_var() -> Shell {
        let env = crate::environment::Environment::new();
        env.write()
            .set_shell_var("DOGESH_EMPTY_PROBE".to_string(), String::new());
        Shell::new(env)
    }

    async fn expand_single_arg(shell: &mut Shell, input: &str) -> Vec<String> {
        let ctx = Context::new_safe(shell.pid, shell.pgid, false);
        let plan = super::super::parse::parse_execution_plan(input, Arc::clone(&shell.environment))
            .expect("plan");
        let word = &plan.lists[0].jobs[0].stages[0].argv[1];
        let mut resources = ExecutionResources::new();
        let mut trace = ExpansionTrace::default();
        expand_argument_word(shell, &ctx, word, allow_all, &mut resources, &mut trace)
            .await
            .expect("expand")
    }

    /// An unquoted empty variable contributes zero fields; only quoted
    /// empties keep one empty argv.
    #[tokio::test]
    async fn unquoted_empty_variable_expands_to_zero_fields() {
        let mut shell = shell_with_empty_var();
        let fields = expand_single_arg(&mut shell, "echo $DOGESH_EMPTY_PROBE").await;
        assert!(fields.is_empty());
    }

    #[tokio::test]
    async fn quoted_empty_variable_keeps_one_empty_field() {
        let mut shell = shell_with_empty_var();
        let fields = expand_single_arg(&mut shell, "echo \"$DOGESH_EMPTY_PROBE\"").await;
        assert_eq!(fields, vec![String::new()]);
    }

    #[tokio::test]
    async fn unset_variable_expands_to_empty_not_literal() {
        let env = crate::environment::Environment::new();
        let mut shell = Shell::new(env);
        shell
            .environment
            .write()
            .unset_shell_var("DOGESH_DEFINITELY_UNSET_PROBE");
        let fields = expand_single_arg(&mut shell, "echo $DOGESH_DEFINITELY_UNSET_PROBE").await;
        assert!(fields.is_empty());
        let fields = expand_single_arg(&mut shell, "echo \"$DOGESH_DEFINITELY_UNSET_PROBE\"").await;
        assert_eq!(fields, vec![String::new()]);
    }

    /// `~user` still resolves through the system user database, matching the
    /// historical `shellexpand` behavior for spellings the shell HOME does
    /// not cover.
    #[test]
    fn tilde_user_matches_system_expansion() {
        let env = crate::environment::Environment::new();
        let shell = Shell::new(env);
        for candidate in ["~root", "~daemon"] {
            let expected = shellexpand::tilde(candidate).into_owned();
            let (text, _) = apply_tilde(candidate, candidate, &shell);
            assert_eq!(text, expected, "for {candidate:?}");
        }
    }

    #[test]
    fn substitution_trims_lf_only() {
        assert_eq!(trim_substitution_output("a\n"), "a");
        assert_eq!(trim_substitution_output("a\n\n\n"), "a");
        assert_eq!(trim_substitution_output("a\r"), "a\r");
        assert_eq!(trim_substitution_output("a\r\n"), "a\r");
        assert_eq!(trim_substitution_output("a\r\n\n"), "a\r");
        assert_eq!(trim_substitution_output("a\rb\nc"), "a\rb\nc");
        assert_eq!(trim_substitution_output("a\nb\r\n"), "a\nb\r");
        assert_eq!(trim_substitution_output(""), "");
        assert_eq!(trim_substitution_output("\n"), "");
        assert_eq!(trim_substitution_output("\r"), "\r");
        assert_eq!(trim_substitution_output("\r\n"), "\r");
        assert_eq!(trim_substitution_output("\n\r"), "\n\r");
        assert_eq!(trim_substitution_output("\r\r\n"), "\r\r");
    }

    /// `PATH` assigned through `${PATH:=...}` refreshes derived lookup state,
    /// exactly like a normal logical `PATH` assignment.
    #[tokio::test]
    async fn path_assign_refreshes_derived_paths() {
        let env = crate::environment::Environment::new();
        let mut shell = Shell::new(env);
        shell.environment.write().unset_shell_var("PATH");
        assert!(shell.environment.read().lookup_variable("PATH").is_none());
        let fields = expand_single_arg(&mut shell, "echo ${PATH:=/foo-bar-test:/baz-test}").await;
        assert_eq!(fields, vec!["/foo-bar-test:/baz-test".to_string()]);
        let guard = shell.environment.read();
        assert_eq!(
            guard.lookup_variable("PATH").as_deref(),
            Some("/foo-bar-test:/baz-test")
        );
        assert_eq!(
            guard.variable_state.paths,
            vec!["/foo-bar-test".to_string(), "/baz-test".to_string()]
        );
    }

    /// `${X:=new}` preserves an existing export attribute; unset creates a
    /// non-exported variable.
    #[tokio::test]
    async fn assign_preserves_export_attribute() {
        let env = crate::environment::Environment::new();
        let mut shell = Shell::new(env);
        // Exported empty stays exported with new value.
        shell
            .environment
            .write()
            .set_shell_var("DOGESH_EXP_PROBE".to_string(), String::new());
        shell
            .environment
            .write()
            .export_shell_var("DOGESH_EXP_PROBE".to_string());
        let fields = expand_single_arg(&mut shell, "echo ${DOGESH_EXP_PROBE:=new}").await;
        assert_eq!(fields, vec!["new".to_string()]);
        {
            let guard = shell.environment.read();
            assert_eq!(
                guard.lookup_variable("DOGESH_EXP_PROBE").as_deref(),
                Some("new")
            );
            assert!(
                guard
                    .variable_state
                    .exported_vars
                    .contains("DOGESH_EXP_PROBE")
            );
        }
        // Unset creates a normal (non-exported) variable.
        shell
            .environment
            .write()
            .unset_shell_var("DOGESH_NEW_PROBE");
        let fields = expand_single_arg(&mut shell, "echo ${DOGESH_NEW_PROBE:=new}").await;
        assert_eq!(fields, vec!["new".to_string()]);
        {
            let guard = shell.environment.read();
            assert_eq!(
                guard.lookup_variable("DOGESH_NEW_PROBE").as_deref(),
                Some("new")
            );
            assert!(
                !guard
                    .variable_state
                    .exported_vars
                    .contains("DOGESH_NEW_PROBE")
            );
        }
    }

    /// Dry preflight never mutates, never spawns, and never throws `:?`.
    #[test]
    fn dry_parameter_expansion_has_no_side_effects() {
        use super::{dry_expand_argument_word, dry_expand_scalar_word};

        let env = crate::environment::Environment::new();
        let shell = Shell::new(env);
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        // Setup: SET=value, EMPTY empty, UNSET absent.
        {
            let mut guard = shell.environment.write();
            guard.set_shell_var("DOGESH_DRY_SET".to_string(), "set-val".to_string());
            guard.set_shell_var("DOGESH_DRY_EMPTY".to_string(), String::new());
            guard.unset_shell_var("DOGESH_DRY_UNSET");
            guard.unset_shell_var("DOGESH_DRY_ASSIGN");
        }
        let parse_word_for = |input: &str| {
            let plan = super::super::parse::parse_execution_plan(
                input,
                std::sync::Arc::clone(&shell.environment),
            )
            .expect("plan");
            plan.lists[0].jobs[0].stages[0].argv[1].clone()
        };
        // Selected branch shape is reasonable; substitution stays placeholder.
        let word = parse_word_for("echo ${DOGESH_DRY_SET:-fallback}");
        assert_eq!(
            dry_expand_argument_word(&word, &shell, &cwd),
            vec!["set-val".to_string()]
        );
        let word = parse_word_for("echo ${DOGESH_DRY_UNSET:-fallback}");
        assert_eq!(
            dry_expand_argument_word(&word, &shell, &cwd),
            vec!["fallback".to_string()]
        );
        // Alternate.
        let word = parse_word_for("echo ${DOGESH_DRY_SET:+alt}");
        assert_eq!(
            dry_expand_argument_word(&word, &shell, &cwd),
            vec!["alt".to_string()]
        );
        let word = parse_word_for("echo ${DOGESH_DRY_UNSET:+alt}");
        assert!(dry_expand_argument_word(&word, &shell, &cwd).is_empty());
        // Assign: dry approximation, no store.
        let word = parse_word_for("echo ${DOGESH_DRY_UNSET:=value}");
        assert_eq!(
            dry_expand_argument_word(&word, &shell, &cwd),
            vec!["value".to_string()]
        );
        assert!(
            shell
                .environment
                .read()
                .lookup_variable("DOGESH_DRY_UNSET")
                .is_none(),
            "dry := must not store"
        );
        // Error: dry never throws.
        let word = parse_word_for("echo ${DOGESH_DRY_UNSET:?message}");
        let _ = dry_expand_argument_word(&word, &shell, &cwd);
        let word = parse_word_for("echo ${DOGESH_DRY_UNSET:?message}");
        assert_eq!(dry_expand_scalar_word(&word, &shell), String::new());
        // No spawn: operand substitution stays placeholder, marker absent.
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("dry_must_not_run");
        let input = format!("echo ${{DOGESH_DRY_UNSET:-$(touch {})}}", marker.display());
        let word = parse_word_for(&input);
        let fields = dry_expand_argument_word(&word, &shell, &cwd);
        assert!(fields.iter().any(|f| f.contains("touch")));
        assert!(!marker.exists());
    }
}
