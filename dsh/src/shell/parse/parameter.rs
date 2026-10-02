//! Structured `${VAR-op}` operand planning.
//!
//! Side-effect-free: pest pairs become [`PlannedParameterExpansion`] data.
//! Never reads variables, assigns, executes substitutions, splits, or globs.

use super::super::plan::{
    ParameterAction, ParameterCondition, PlannedLiteral, PlannedParameterExpansion, PlannedWord,
    ProcessSubstitutionDirection, QuoteMode, WordPart,
};
use super::ParseContext;
use super::{parse_double_quoted_parts, substitution_from_wrapper};
use crate::parser::{self, Rule};
use anyhow::Result;
use pest::iterators::Pair;

/// Map operator spelling to its two semantic axes. Pure syntax, no lookup.
fn parameter_operator_parts(op: &str) -> Result<(ParameterCondition, ParameterAction)> {
    match op {
        ":-" => Ok((ParameterCondition::UnsetOrNull, ParameterAction::Default)),
        "-" => Ok((ParameterCondition::UnsetOnly, ParameterAction::Default)),
        ":=" => Ok((ParameterCondition::UnsetOrNull, ParameterAction::Assign)),
        "=" => Ok((ParameterCondition::UnsetOnly, ParameterAction::Assign)),
        ":?" => Ok((ParameterCondition::UnsetOrNull, ParameterAction::Error)),
        "?" => Ok((ParameterCondition::UnsetOnly, ParameterAction::Error)),
        ":+" => Ok((ParameterCondition::UnsetOrNull, ParameterAction::Alternate)),
        "+" => Ok((ParameterCondition::UnsetOnly, ParameterAction::Alternate)),
        _ => anyhow::bail!("unsupported parameter operator: {op:?}"),
    }
}

/// One `${NAME<op>word}` becomes structured plan data.
///
/// Never inspects `NAME`, assigns, executes `word`, splits, or globs.
pub(super) fn parse_parameter_expansion(
    pair: Pair<Rule>,
    ctx: &ParseContext,
    quote: QuoteMode,
) -> Result<WordPart> {
    let mut name = String::new();
    let mut condition = ParameterCondition::UnsetOnly;
    let mut action = ParameterAction::Default;
    let mut word: Option<Box<PlannedWord>> = None;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::parameter_name => name = inner.as_str().to_string(),
            Rule::parameter_operator => {
                let (cond, act) = parameter_operator_parts(inner.as_str())?;
                condition = cond;
                action = act;
            }
            Rule::parameter_word => {
                word = Some(Box::new(parse_parameter_word(inner, ctx)?));
            }
            _ => {}
        }
    }
    if name.is_empty() {
        anyhow::bail!("parameter expansion is missing a name");
    }
    Ok(WordPart::ParameterExpansion {
        expansion: PlannedParameterExpansion {
            name,
            condition,
            action,
            word,
        },
        quote,
    })
}

/// Operand `word` inside `${VAR:-word}`: parsed recursively, never executed.
///
/// `parameter_text` (no backslashes) becomes an unquoted operand literal that
/// is splittable in `ParameterOperand` context. `escape_sequence` (`\x`)
/// becomes a protected (`Single`-quoted) literal so `${X:-a\ b}` stays one
/// field while `${X:-a b}` may split. Quoted/nested dynamics keep their own
/// quote modes.
pub(super) fn parse_parameter_word(word: Pair<Rule>, ctx: &ParseContext) -> Result<PlannedWord> {
    let source = word.as_str().to_string();
    let mut parts = Vec::new();
    for part in word.into_inner() {
        match part.as_rule() {
            Rule::arithmetic_expansion => {
                parts.push(super::arithmetic::parse_arithmetic_expansion(
                    part,
                    ctx,
                    QuoteMode::Unquoted,
                )?);
            }
            Rule::parameter_expansion => {
                parts.push(parse_parameter_expansion(part, ctx, QuoteMode::Unquoted)?);
            }
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
                parts.extend(parse_double_quoted_parts(part, ctx)?);
            }
            Rule::command_subst => {
                for subst in substitution_from_wrapper(
                    part,
                    super::super::plan::PlannedSubstitutionKind::Command,
                    ctx,
                )? {
                    parts.push(WordPart::Substitution {
                        substitution: subst,
                        quote: QuoteMode::Unquoted,
                    });
                }
            }
            Rule::proc_subst => {
                for subst in substitution_from_wrapper(
                    part,
                    super::super::plan::PlannedSubstitutionKind::Process(
                        ProcessSubstitutionDirection::Read,
                    ),
                    ctx,
                )? {
                    parts.push(WordPart::Substitution {
                        substitution: subst,
                        quote: QuoteMode::Unquoted,
                    });
                }
            }
            Rule::subshell => {
                for subst in substitution_from_wrapper(
                    part,
                    super::super::plan::PlannedSubstitutionKind::Subshell,
                    ctx,
                )? {
                    parts.push(WordPart::Substitution {
                        substitution: subst,
                        quote: QuoteMode::Unquoted,
                    });
                }
            }
            Rule::invocation_parameter => parts.push(super::parse_invocation_parameter(
                part.as_str(),
                QuoteMode::Unquoted,
            )?),
            Rule::variable => parts.push(WordPart::Variable {
                source: part.as_str().to_string(),
                quote: QuoteMode::Unquoted,
            }),
            Rule::escape_sequence => {
                let raw = part.as_str().to_string();
                let text = raw.get(1..).unwrap_or_default().to_string();
                parts.push(WordPart::Literal(PlannedLiteral {
                    text,
                    raw,
                    quote: QuoteMode::Single,
                    pattern_active: false,
                    brace_active: false,
                    tilde_candidate: false,
                }));
            }
            Rule::parameter_text => {
                let raw = part.as_str().to_string();
                parts.push(WordPart::Literal(PlannedLiteral {
                    text: raw.clone(),
                    raw,
                    quote: QuoteMode::Unquoted,
                    pattern_active: false,
                    brace_active: false,
                    tilde_candidate: false,
                }));
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
    }
    Ok(PlannedWord { source, parts })
}
