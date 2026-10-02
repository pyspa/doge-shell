//! Structured `${VAR-op}` runtime orchestration.
//!
//! Splices selected operands into the outer `ExpandedSegment` stream with
//! lazy evaluation, assignment through the logical environment, and typed
//! `:?` failures. Pure state/decision lives in `super::parameter_expand`;
//! segment/scalar assembly lives here so `word_expand` stays under budget.

use super::authorize::ConfirmFn;
use super::field_split::{ExpandedSegment, PatternKind};
use super::parameter_expand::{
    ParameterDecision, ParameterExpansionError, decide_parameter_expansion,
};
use super::plan::{
    PlannedParameterExpansion, PlannedSubstitutionKind, PlannedWord, QuoteMode, WordPart,
};
use super::process_substitution::{ExecutionResources, start_process_substitution};
use super::substitution::capture_subshell_plan_stdout;
use super::word_expand::{
    ExpansionContext, ExpansionTrace, expand_word_segments, literal_segment, resolve_parameter,
    trim_substitution_output, variable_segment,
};
use super::word_expand_invocation::invocation_scalar;
use crate::parser::expansion::{escape_brace_metacharacters, escape_glob_metacharacters};
use crate::process::reexec::PlanExecMode;
use crate::shell::expansion_host::ExpansionHost;
use anyhow::Result;
use dsh_types::Context;
use std::future::Future;
use std::pin::Pin;

/// Named parameter state for structured `${VAR-op}`: bare name only, never
/// the full `${VAR:-word}` spelling.
fn named_parameter_state(
    name: &str,
    shell: &impl ExpansionHost,
) -> super::parameter_expand::ParameterState {
    use super::parameter_expand::ParameterState;
    match shell.expansion_environment().read().lookup_variable(name) {
        Some(value) => ParameterState::set(value),
        None => ParameterState::unset(),
    }
}

/// Splice one `${VAR-op}` into outer segments.
///
/// The decision is made before any operand expansion, so unselected `word`
/// never executes substitutions, assigns, or authorizes. Selected operands
/// share the same mutable trace in left-to-right order.
pub(crate) fn expand_parameter_to_segments<'a>(
    shell: &'a mut impl ExpansionHost,
    ctx: &'a Context,
    expansion: &'a PlannedParameterExpansion,
    quote: QuoteMode,
    confirm: ConfirmFn,
    resources: &'a mut ExecutionResources,
    trace: &'a mut ExpansionTrace,
) -> Pin<Box<dyn Future<Output = Result<Vec<ExpandedSegment>>> + 'a>> {
    Box::pin(async move {
        let state = named_parameter_state(&expansion.name, shell);
        let decision = decide_parameter_expansion(&state, expansion.condition, expansion.action);
        let outer_quoted = quote != QuoteMode::Unquoted;
        match decision {
            ParameterDecision::UseParameter => {
                if outer_quoted {
                    Ok(vec![ExpandedSegment::protected(
                        state.value.clone(),
                        escape_glob_metacharacters(&state.value),
                        PatternKind::Inactive,
                        true,
                    )])
                } else {
                    Ok(vec![ExpandedSegment::splittable_dynamic(
                        state.value.clone(),
                        escape_brace_metacharacters(&state.value),
                    )])
                }
            }
            ParameterDecision::UseWord => {
                let Some(operand) = expansion.word.as_deref() else {
                    if outer_quoted {
                        return Ok(vec![ExpandedSegment::protected(
                            String::new(),
                            String::new(),
                            PatternKind::Inactive,
                            true,
                        )]);
                    }
                    return Ok(Vec::new());
                };
                let operand_segments = expand_word_segments(
                    shell,
                    ctx,
                    operand,
                    confirm,
                    resources,
                    trace,
                    ExpansionContext::ParameterOperand,
                )
                .await?;
                if outer_quoted {
                    // Outer double quotes override splitting/globbing: join
                    // the selected operand into one protected field. Nested
                    // substitutions already executed above.
                    let mut text = String::new();
                    for seg in &operand_segments {
                        text.push_str(&seg.text);
                    }
                    Ok(vec![ExpandedSegment::protected(
                        text.clone(),
                        escape_glob_metacharacters(&text),
                        PatternKind::Inactive,
                        true,
                    )])
                } else {
                    Ok(operand_segments)
                }
            }
            ParameterDecision::AssignWord => {
                let scalar = match expansion.word.as_deref() {
                    Some(operand) => {
                        expand_operand_scalar(shell, ctx, operand, confirm, resources, trace)
                            .await?
                    }
                    None => String::new(),
                };
                // Through the logical environment so `PATH` refreshes derived
                // state and existing export attributes are preserved.
                shell
                    .expansion_environment()
                    .write()
                    .set_shell_var(expansion.name.clone(), scalar.clone());
                if outer_quoted {
                    Ok(vec![ExpandedSegment::protected(
                        scalar.clone(),
                        escape_glob_metacharacters(&scalar),
                        PatternKind::Inactive,
                        true,
                    )])
                } else {
                    // Substitute the newly assigned value as an ordinary
                    // unquoted parameter: eligible for IFS splitting, unlike
                    // the `:-` operand provenance which keeps escapes.
                    Ok(vec![ExpandedSegment::splittable_dynamic(
                        scalar.clone(),
                        escape_brace_metacharacters(&scalar),
                    )])
                }
            }
            ParameterDecision::Error => {
                let message = match expansion.word.as_deref() {
                    Some(operand) => {
                        expand_operand_scalar(shell, ctx, operand, confirm, resources, trace)
                            .await?
                    }
                    None => default_parameter_error_message(expansion.condition),
                };
                Err(anyhow::anyhow!(ParameterExpansionError {
                    parameter: expansion.name.clone(),
                    message,
                }))
            }
            ParameterDecision::UseNull => {
                if outer_quoted {
                    Ok(vec![ExpandedSegment::protected(
                        String::new(),
                        String::new(),
                        PatternKind::Inactive,
                        true,
                    )])
                } else {
                    Ok(Vec::new())
                }
            }
        }
    })
}

pub(crate) fn default_parameter_error_message(
    condition: super::plan::ParameterCondition,
) -> String {
    use super::plan::ParameterCondition;
    match condition {
        ParameterCondition::UnsetOrNull => "parameter null or not set".to_string(),
        ParameterCondition::UnsetOnly => "parameter not set".to_string(),
    }
}

/// Scalar operand expansion for `:=` assignment and `:?` diagnostics.
///
/// Quote removal as structured, no IFS splitting, no pathname expansion.
/// Nested `${...}` may assign or fail recursively; the same trace observes
/// selected substitutions in order. Boxed for recursion.
pub(crate) fn expand_operand_scalar<'a>(
    shell: &'a mut impl ExpansionHost,
    ctx: &'a Context,
    word: &'a PlannedWord,
    confirm: ConfirmFn,
    resources: &'a mut ExecutionResources,
    trace: &'a mut ExpansionTrace,
) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>> {
    Box::pin(async move {
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
                    let scalar = expand_parameter_to_scalar(
                        shell, ctx, expansion, *quote, confirm, resources, trace,
                    )
                    .await?;
                    // Scalar assignment context: the outer quote of a nested
                    // expansion does not create extra fields; join literally.
                    // Quoting inside the operand already shaped `scalar`.
                    let _ = quote;
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
    })
}

/// Scalar `${VAR-op}` for assignment/redirect/error paths.
///
/// Never splits or globs. `AssignWord` mutates through the environment;
/// `Error` returns the typed failure after lazily expanding its diagnostic.
pub(crate) fn expand_parameter_to_scalar<'a>(
    shell: &'a mut impl ExpansionHost,
    ctx: &'a Context,
    expansion: &'a PlannedParameterExpansion,
    _quote: QuoteMode,
    confirm: ConfirmFn,
    resources: &'a mut ExecutionResources,
    trace: &'a mut ExpansionTrace,
) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>> {
    Box::pin(async move {
        let state = named_parameter_state(&expansion.name, shell);
        let decision = decide_parameter_expansion(&state, expansion.condition, expansion.action);
        match decision {
            ParameterDecision::UseParameter => Ok(state.value),
            ParameterDecision::UseWord => {
                let Some(operand) = expansion.word.as_deref() else {
                    return Ok(String::new());
                };
                expand_operand_scalar(shell, ctx, operand, confirm, resources, trace).await
            }
            ParameterDecision::AssignWord => {
                let scalar = match expansion.word.as_deref() {
                    Some(operand) => {
                        expand_operand_scalar(shell, ctx, operand, confirm, resources, trace)
                            .await?
                    }
                    None => String::new(),
                };
                shell
                    .expansion_environment()
                    .write()
                    .set_shell_var(expansion.name.clone(), scalar.clone());
                Ok(scalar)
            }
            ParameterDecision::Error => {
                let message = match expansion.word.as_deref() {
                    Some(operand) => {
                        expand_operand_scalar(shell, ctx, operand, confirm, resources, trace)
                            .await?
                    }
                    None => default_parameter_error_message(expansion.condition),
                };
                Err(anyhow::anyhow!(ParameterExpansionError {
                    parameter: expansion.name.clone(),
                    message,
                }))
            }
            ParameterDecision::UseNull => Ok(String::new()),
        }
    })
}

/// Dry parameter expansion: same branch selection, no mutation, no spawn,
/// never a real `:?` error. Later words cannot observe a hypothetical `:=`.
pub(crate) fn dry_parameter_to_segments(
    expansion: &PlannedParameterExpansion,
    quote: QuoteMode,
    shell: &impl ExpansionHost,
) -> Vec<ExpandedSegment> {
    let state = named_parameter_state(&expansion.name, shell);
    let decision = decide_parameter_expansion(&state, expansion.condition, expansion.action);
    let outer_quoted = quote != QuoteMode::Unquoted;
    match decision {
        ParameterDecision::UseParameter => {
            if outer_quoted {
                vec![ExpandedSegment::protected(
                    state.value.clone(),
                    escape_glob_metacharacters(&state.value),
                    PatternKind::Inactive,
                    true,
                )]
            } else {
                vec![ExpandedSegment::splittable_dynamic(
                    state.value.clone(),
                    escape_brace_metacharacters(&state.value),
                )]
            }
        }
        ParameterDecision::UseWord => {
            let Some(operand) = expansion.word.as_deref() else {
                if outer_quoted {
                    return vec![ExpandedSegment::protected(
                        String::new(),
                        String::new(),
                        PatternKind::Inactive,
                        true,
                    )];
                }
                return Vec::new();
            };
            let operand_segments = dry_operand_segments(operand, shell);
            if outer_quoted {
                let mut text = String::new();
                for seg in &operand_segments {
                    text.push_str(&seg.text);
                }
                vec![ExpandedSegment::protected(
                    text.clone(),
                    escape_glob_metacharacters(&text),
                    PatternKind::Inactive,
                    true,
                )]
            } else {
                operand_segments
            }
        }
        ParameterDecision::AssignWord => {
            // Dry approximation: expand the would-be value, use it now,
            // never store it.
            let approx = expansion
                .word
                .as_deref()
                .map(|operand| dry_operand_scalar(operand, shell))
                .unwrap_or_default();
            if outer_quoted {
                vec![ExpandedSegment::protected(
                    approx.clone(),
                    escape_glob_metacharacters(&approx),
                    PatternKind::Inactive,
                    true,
                )]
            } else {
                vec![ExpandedSegment::splittable_dynamic(
                    approx.clone(),
                    escape_brace_metacharacters(&approx),
                )]
            }
        }
        ParameterDecision::Error | ParameterDecision::UseNull => {
            // Dry preflight never throws `:?` into the live shell. Shape it
            // like null so the projection stays fail-closed without
            // aborting inspection; live materialization remains authoritative.
            if outer_quoted {
                vec![ExpandedSegment::protected(
                    String::new(),
                    String::new(),
                    PatternKind::Inactive,
                    true,
                )]
            } else {
                Vec::new()
            }
        }
    }
}

/// Dry operand segments: unquoted text splittable, escaped/quoted protected,
/// nested dynamics without spawning.
pub(crate) fn dry_operand_segments(
    word: &PlannedWord,
    shell: &impl ExpansionHost,
) -> Vec<ExpandedSegment> {
    let mut segments = Vec::with_capacity(word.parts.len());
    for part in &word.parts {
        match part {
            WordPart::Literal(literal) => {
                // Mirror live `ParameterOperand`: unquoted splittable,
                // otherwise protected. No tilde.
                match literal.quote {
                    QuoteMode::Unquoted => segments.push(ExpandedSegment::splittable_dynamic(
                        literal.text.clone(),
                        escape_brace_metacharacters(&literal.text),
                    )),
                    _ => segments.push(ExpandedSegment::protected(
                        literal.text.clone(),
                        escape_glob_metacharacters(&literal.text),
                        PatternKind::Inactive,
                        !literal.text.is_empty() || literal.quote != QuoteMode::Unquoted,
                    )),
                }
            }
            WordPart::InvocationParameter { parameter, quote } => {
                segments.extend(super::word_expand_invocation::invocation_segments(
                    *parameter,
                    *quote,
                    shell,
                    ExpansionContext::DryArgument,
                ));
            }
            WordPart::Variable { source, quote } => {
                segments.push(variable_segment(
                    source,
                    *quote,
                    shell,
                    ExpansionContext::DryArgument,
                ));
            }
            WordPart::ParameterExpansion { expansion, quote } => {
                segments.extend(dry_parameter_to_segments(expansion, *quote, shell));
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
                segments.push(ExpandedSegment::protected(
                    placeholder.clone(),
                    escape_glob_metacharacters(&placeholder),
                    PatternKind::Inactive,
                    quoted || !placeholder.is_empty(),
                ));
            }
        }
    }
    segments
}

pub(crate) fn dry_operand_scalar(word: &PlannedWord, shell: &impl ExpansionHost) -> String {
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
                out.push_str(&dry_parameter_to_scalar(expansion, shell));
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

pub(crate) fn dry_parameter_to_scalar(
    expansion: &PlannedParameterExpansion,
    shell: &impl ExpansionHost,
) -> String {
    let state = named_parameter_state(&expansion.name, shell);
    let decision = decide_parameter_expansion(&state, expansion.condition, expansion.action);
    match decision {
        ParameterDecision::UseParameter => state.value,
        ParameterDecision::UseWord | ParameterDecision::AssignWord => expansion
            .word
            .as_deref()
            .map(|operand| dry_operand_scalar(operand, shell))
            .unwrap_or_default(),
        ParameterDecision::Error | ParameterDecision::UseNull => String::new(),
    }
}
