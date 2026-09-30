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

use super::authorize::ConfirmFn;
use super::field_split::{ExpandedSegment, IfsSpec, PatternKind, SplitField, split_segments};
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
use std::path::PathBuf;

/// Set/unset-preserving parameter resolution.
///
/// Ordinary `$FOO` maps both unset and set-empty to an empty value, but the
/// distinction is kept for the upcoming `${FOO-default}` family. Never fall
/// back to the literal source spelling for unset parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedParameter {
    value: String,
    is_set: bool,
}

fn resolve_parameter(source: &str, shell: &Shell) -> ResolvedParameter {
    match shell.environment.read().get_var(source) {
        Some(value) => ResolvedParameter {
            value,
            is_set: true,
        },
        None => ResolvedParameter {
            value: String::new(),
            is_set: false,
        },
    }
}

/// Which expansion semantics apply to one word.
///
/// Argument words field-split on IFS and allow unquoted dynamics to glob.
/// Assignment right-hand sides are scalar (no split, no glob). Redirect
/// targets are scalar for splitting (a variable containing spaces stays one
/// target) but keep source glob behaviour so existing ambiguous-wildcard
/// contracts hold. DryArgument mirrors Argument without executing bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpansionContext {
    Argument,
    Assignment,
    Redirect,
    DryArgument,
}

impl ExpansionContext {
    fn do_field_split(self) -> bool {
        matches!(
            self,
            ExpansionContext::Argument | ExpansionContext::DryArgument
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

fn literal_segment(
    literal: &PlannedLiteral,
    shell: &Shell,
    first: bool,
    ctx: ExpansionContext,
) -> ExpandedSegment {
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

fn variable_segment(
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
        ExpansionContext::Argument | ExpansionContext::DryArgument => {
            if quoted {
                ExpandedSegment::protected(
                    resolved.value.clone(),
                    escape_glob_metacharacters(&resolved.value),
                    PatternKind::Inactive,
                    true,
                )
            } else {
                // Unquoted: split-eligible, glob-active, brace-inactive.
                // `is_set` is retained in `ResolvedParameter` for the
                // upcoming `${VAR-op}` task; both states map to empty here.
                let _ = resolved.is_set;
                ExpandedSegment::splittable_dynamic(
                    resolved.value.clone(),
                    escape_brace_metacharacters(&resolved.value),
                )
            }
        }
    }
}

fn dynamic_text_segment(value: String, quoted: bool, ctx: ExpansionContext) -> ExpandedSegment {
    match ctx {
        ExpansionContext::Assignment | ExpansionContext::Redirect => {
            ExpandedSegment::protected(value, String::new(), PatternKind::Inactive, true)
        }
        // Argument/DryArgument: quoted stays protected; unquoted is
        // split-eligible with glob active and braces inactive.
        ExpansionContext::Argument | ExpansionContext::DryArgument => {
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

fn trim_substitution_output(output: &str) -> String {
    output.trim_end_matches(['\n', '\r']).to_string()
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
}
