//! Structured `$((...))` body planning.
//!
//! Side-effect-free: pest pairs become [`PlannedArithmeticExpansion`] data.
//! Never reads variables, evaluates arithmetic, executes substitutions,
//! assigns, splits, or globs. The body stays a [`PlannedWord`] so runtime
//! scalar expansion can resolve `$VAR` / `${...}` / `$(...)` / nested
//! `$((...))` through the existing mechanisms before arithmetic evaluation.

use super::super::plan::{
    PlannedArithmeticExpansion, PlannedLiteral, PlannedWord, QuoteMode, WordPart,
};
use super::ParseContext;
use super::{parse_double_quoted_parts, substitution_from_wrapper};
use crate::parser::{self, Rule};
use anyhow::Result;
use pest::iterators::Pair;

/// One `$((...))` becomes structured plan data.
///
/// `quote` is the outer word context (`Unquoted` for bare words, `Double`
/// inside `"..."`). The body itself is always scalar at runtime (no
/// splitting/globbing), so inner dynamic parts keep `Unquoted` quote modes;
/// double-quoted strings inside keep their own `Double` provenance.
pub(super) fn parse_arithmetic_expansion(
    pair: Pair<Rule>,
    ctx: &ParseContext,
    quote: QuoteMode,
) -> Result<WordPart> {
    let full = pair.as_str().to_string();
    let source = strip_delimiters(&full);
    let mut parts = Vec::new();
    for inner in pair.into_inner() {
        push_arithmetic_inner(inner, ctx, &mut parts)?;
    }
    Ok(WordPart::ArithmeticExpansion {
        expansion: PlannedArithmeticExpansion {
            source,
            body: Box::new(PlannedWord {
                source: full.clone(),
                parts,
            }),
        },
        quote,
    })
}

fn strip_delimiters(full: &str) -> String {
    full.strip_prefix("$((")
        .and_then(|rest| rest.strip_suffix("))"))
        .unwrap_or(full)
        .to_string()
}

fn push_arithmetic_inner(
    pair: Pair<Rule>,
    ctx: &ParseContext,
    parts: &mut Vec<WordPart>,
) -> Result<()> {
    match pair.as_rule() {
        Rule::arithmetic_expansion => {
            parts.push(parse_arithmetic_expansion(pair, ctx, QuoteMode::Unquoted)?);
        }
        Rule::parameter_expansion => {
            parts.push(super::parameter::parse_parameter_expansion(
                pair,
                ctx,
                QuoteMode::Unquoted,
            )?);
        }
        Rule::variable => parts.push(WordPart::Variable {
            source: pair.as_str().to_string(),
            quote: QuoteMode::Unquoted,
        }),
        Rule::command_subst => {
            for subst in substitution_from_wrapper(
                pair,
                super::super::plan::PlannedSubstitutionKind::Command,
                ctx,
            )? {
                parts.push(WordPart::Substitution {
                    substitution: subst,
                    quote: QuoteMode::Unquoted,
                });
            }
        }
        Rule::s_quoted => {
            let raw = pair.as_str().to_string();
            let text = parser::get_string(pair).unwrap_or_default();
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
            parts.extend(parse_double_quoted_parts(pair, ctx)?);
        }
        Rule::arithmetic_paren => {
            // Preserve grouping parentheses as literal text around the
            // recursively planned inner content.
            parts.push(WordPart::Literal(PlannedLiteral {
                text: "(".to_string(),
                raw: "(".to_string(),
                quote: QuoteMode::Unquoted,
                pattern_active: false,
                brace_active: false,
                tilde_candidate: false,
            }));
            for inner in pair.into_inner() {
                push_arithmetic_inner(inner, ctx, parts)?;
            }
            parts.push(WordPart::Literal(PlannedLiteral {
                text: ")".to_string(),
                raw: ")".to_string(),
                quote: QuoteMode::Unquoted,
                pattern_active: false,
                brace_active: false,
                tilde_candidate: false,
            }));
        }
        Rule::escape_sequence => {
            let raw = pair.as_str().to_string();
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
        Rule::arithmetic_text => {
            let raw = pair.as_str().to_string();
            parts.push(WordPart::Literal(PlannedLiteral {
                text: raw.clone(),
                raw,
                quote: QuoteMode::Unquoted,
                pattern_active: false,
                brace_active: false,
                tilde_candidate: false,
            }));
        }
        other => {
            // `quoted` is silent so it never arrives here, and
            // `arithmetic_inner` enumerates every valid child. Anything else
            // (e.g. a future `proc_subst` / `subshell` variant leaking into
            // an arithmetic body) must fail closed: pushing it as a literal
            // would bypass structured authorization at expansion time.
            anyhow::bail!("unsupported arithmetic construct: {other:?}");
        }
    }
    Ok(())
}
