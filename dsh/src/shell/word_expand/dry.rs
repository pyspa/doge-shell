//! Read-only safety preflight expansion; substitution bodies stay placeholders.

use super::{
    ExpansionContext, finish_split_fields, literal_segment, resolve_ifs, resolve_parameter,
    variable_segment,
};
use crate::parser::expansion::escape_glob_metacharacters;
use crate::shell::expansion_host::ExpansionHost;
use crate::shell::field_split::{ExpandedSegment, PatternKind, split_segments};
use crate::shell::plan::{
    PlannedSubstitutionKind, PlannedWord, ProcessSubstitutionDirection, QuoteMode, WordPart,
};
use crate::shell::word_expand_invocation::{invocation_scalar, invocation_segments};
use crate::shell::{word_expand_arithmetic, word_expand_param};

/// Read-only expansion for safety preflight: no spawn, no env mutation.
///
/// Mirrors [`super::expand_argument_word`] with the same IFS semantics for current
/// variable values; substitution bodies stay diagnostic placeholders.
pub fn dry_expand_argument_word(
    word: &PlannedWord,
    shell: &impl ExpansionHost,
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
            WordPart::InvocationParameter { parameter, quote } => {
                segments.extend(invocation_segments(*parameter, *quote, shell, context));
            }
            WordPart::Variable { source, quote } => {
                segments.push(variable_segment(source, *quote, shell, context));
            }
            WordPart::ParameterExpansion { expansion, quote } => {
                segments.extend(word_expand_param::dry_parameter_to_segments(
                    expansion, *quote, shell,
                ));
            }
            WordPart::ArithmeticExpansion { expansion, quote } => {
                segments.extend(word_expand_arithmetic::dry_arithmetic_to_segments(
                    expansion, *quote, shell,
                ));
            }
            WordPart::Substitution {
                substitution,
                quote,
            } => {
                let placeholder = match substitution.kind {
                    PlannedSubstitutionKind::Command => format!("$({})", substitution.source),
                    PlannedSubstitutionKind::Process(ProcessSubstitutionDirection::Read) => {
                        format!("<({})", substitution.source)
                    }
                    PlannedSubstitutionKind::Process(ProcessSubstitutionDirection::Write) => {
                        format!(">({})", substitution.source)
                    }
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
pub fn dry_expand_scalar_word(word: &PlannedWord, shell: &impl ExpansionHost) -> String {
    let mut out = String::new();
    let mut first_part = true;
    for part in &word.parts {
        match part {
            WordPart::Literal(literal) => {
                let seg = literal_segment(literal, shell, first_part, ExpansionContext::Assignment);
                out.push_str(&seg.text);
            }
            WordPart::InvocationParameter { parameter, .. } => {
                out.push_str(&invocation_scalar(*parameter, shell));
            }
            WordPart::Variable { source, .. } => {
                out.push_str(&resolve_parameter(source, shell).value);
            }
            WordPart::ParameterExpansion { expansion, .. } => {
                out.push_str(&word_expand_param::dry_parameter_to_scalar(
                    expansion, shell,
                ));
            }
            WordPart::ArithmeticExpansion { expansion, .. } => {
                out.push_str(&word_expand_arithmetic::dry_arithmetic_to_scalar(
                    expansion, shell,
                ));
            }
            WordPart::Substitution { substitution, .. } => match substitution.kind {
                PlannedSubstitutionKind::Command => {
                    out.push_str(&format!("$({})", substitution.source));
                }
                PlannedSubstitutionKind::Process(ProcessSubstitutionDirection::Read) => {
                    out.push_str(&format!("<({})", substitution.source));
                }
                PlannedSubstitutionKind::Process(ProcessSubstitutionDirection::Write) => {
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
