//! Scalar redirection targets and ambiguous pathname expansion errors.

use super::{
    ExpansionContext, ExpansionTrace, cwd_for_expansion, finish_split_fields, literal_segment,
    trim_substitution_output, variable_segment,
};
use crate::parser::expansion::escape_glob_metacharacters;
use crate::process::reexec::PlanExecMode;
use crate::shell::authorize::ConfirmFn;
use crate::shell::expansion_host::ExpansionHost;
use crate::shell::field_split::{PatternKind, SplitField};
use crate::shell::plan::{PlannedSubstitutionKind, PlannedWord, WordPart};
use crate::shell::process_substitution::{ExecutionResources, start_process_substitution};
use crate::shell::substitution::capture_subshell_plan_stdout;
use crate::shell::word_expand_invocation::invocation_scalar;
use crate::shell::{word_expand_arithmetic, word_expand_param};
use anyhow::Result;
use dsh_types::Context;

/// A redirect target that did not expand to exactly one field.
///
/// Typed so materialization turns it into an ordinary failure of that one
/// command (status 1) instead of aborting the whole list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AmbiguousRedirectError {
    source: String,
    fields: usize,
}

impl AmbiguousRedirectError {
    fn new(source: &str, fields: usize) -> Self {
        Self {
            source: source.to_string(),
            fields,
        }
    }
}

impl std::fmt::Display for AmbiguousRedirectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ambiguous redirect: '{}' expands to {} fields",
            self.source, self.fields
        )
    }
}

impl std::error::Error for AmbiguousRedirectError {}

/// Expand a redirect target into exactly one path.
///
/// Scalar for IFS splitting (a variable containing spaces stays one
/// target) while preserving source glob behaviour for ambiguous-wildcard
/// detection. Unquoted dynamics stay protected from both splitting and
/// pathname expansion, matching the pre-existing redirect contract.
pub async fn expand_redirect_target(
    shell: &mut impl ExpansionHost,
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
            WordPart::InvocationParameter { parameter, .. } => {
                let scalar = invocation_scalar(*parameter, shell);
                text.push_str(&scalar);
                pattern.push_str(&escape_glob_metacharacters(&scalar));
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
                let scalar = word_expand_param::expand_parameter_to_scalar(
                    shell, ctx, expansion, *quote, confirm, resources, trace,
                )
                .await?;
                text.push_str(&scalar);
                pattern.push_str(&escape_glob_metacharacters(&scalar));
            }
            WordPart::ArithmeticExpansion { expansion, .. } => {
                // Scalar redirect contract: decimal text, dynamics literal.
                let scalar = word_expand_arithmetic::expand_arithmetic_to_decimal(
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
                        trace.last_command_substitution_status = Some(captured.exit_code);
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
        return Err(AmbiguousRedirectError::new(&word.source, 0).into());
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
        return Err(AmbiguousRedirectError::new(&word.source, fields.len()).into());
    }
    Ok(fields.into_iter().next().expect("one field"))
}
