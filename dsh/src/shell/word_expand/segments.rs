//! Segment provenance, quote protection, IFS lookup, and pathname expansion.

use crate::parser::expansion::{
    escape_brace_metacharacters, escape_glob_metacharacters, expand_braces, expand_glob_pattern,
    unescape_glob_metacharacters,
};
use crate::shell::expansion_host::ExpansionHost;
use crate::shell::field_split::{ExpandedSegment, IfsSpec, PatternKind, SplitField};
use crate::shell::parameter_expand::ParameterState;
use crate::shell::plan::{PlannedLiteral, QuoteMode};

/// Set/unset-preserving parameter resolution.
///
/// Ordinary `$FOO` maps both unset and set-empty to an empty value, but the
/// distinction is kept for `${FOO-default}`. Never fall back to the literal
/// source spelling for unset parameters.
pub(crate) fn resolve_parameter(source: &str, shell: &impl ExpansionHost) -> ParameterState {
    match shell.expansion_environment().read().get_var(source) {
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
    pub(super) fn do_field_split(self) -> bool {
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

fn lookup_home(shell: &impl ExpansionHost) -> Option<String> {
    shell.expansion_environment().read().lookup_variable("HOME")
}

pub(super) fn apply_tilde(
    text: &str,
    pattern: &str,
    shell: &impl ExpansionHost,
) -> (String, String) {
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

pub(super) fn resolve_ifs(shell: &impl ExpansionHost) -> IfsSpec {
    let value = shell.expansion_environment().read().lookup_variable("IFS");
    IfsSpec::resolve(value.as_deref())
}

pub(crate) fn literal_segment(
    literal: &PlannedLiteral,
    shell: &impl ExpansionHost,
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
    shell: &impl ExpansionHost,
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

pub(super) fn finish_split_fields(fields: &[SplitField], cwd: &std::path::Path) -> Vec<String> {
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
