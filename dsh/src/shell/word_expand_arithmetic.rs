//! Runtime `$((...))` expansion for selected jobs.
//!
//! The body [`PlannedWord`] is expanded as scalar (no splitting, globbing,
//! brace or tilde handling) through the existing mechanisms, then parsed as
//! arithmetic and evaluated against the logical [`Environment`]. Nested
//! `$((...))` bodies are evaluated eagerly during the outer body expansion,
//! matching shell expansion order; arithmetic-level `&&` / `||` / `?:`
//! short-circuit lives in the evaluator.

use super::arithmetic::evaluate_expression;
use super::authorize::ConfirmFn;
use super::field_split::{ExpandedSegment, PatternKind};
use super::plan::{PlannedArithmeticExpansion, PlannedSubstitutionKind, PlannedWord, QuoteMode};
use super::process_substitution::{ExecutionResources, start_process_substitution};
use super::substitution::capture_subshell_plan_stdout;
use super::word_expand::{
    ExpansionContext, ExpansionTrace, dynamic_text_segment, literal_segment,
    trim_substitution_output, variable_segment,
};
use crate::parser::expansion::escape_glob_metacharacters;
use crate::process::reexec::PlanExecMode;
use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;
use std::future::Future;
use std::pin::Pin;

/// Expand one arithmetic body to its decimal result.
///
/// Scalar body expansion first (existing `$VAR` / `${...}` / `$(...)` /
/// nested `$((...))` paths, no split/glob), then arithmetic parse + eval.
/// Body-level expansions are eager per shell expansion order: a nested
/// `$((...))`, `$(...)`, or `${...:=...}` runs even when it sits inside a
/// skipped outer `&&` / `||` / `?:` branch (e.g. `$((1 || $(touch f)))`
/// runs `touch`). Short-circuit applies only to arithmetic-level operands
/// such as `(1 / 0)` or `(X = 9)`, which the evaluator skips lazily.
/// Assignments mutate the current shell environment; errors are typed.
pub(crate) fn expand_arithmetic_to_decimal<'a>(
    shell: &'a mut Shell,
    ctx: &'a Context,
    expansion: &'a PlannedArithmeticExpansion,
    confirm: ConfirmFn,
    resources: &'a mut ExecutionResources,
    trace: &'a mut ExpansionTrace,
) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>> {
    Box::pin(async move {
        let expanded =
            expand_body_scalar(shell, ctx, &expansion.body, confirm, resources, trace).await?;
        let value = evaluate_expression(&expansion.source, &expanded, shell)
            .map_err(|err| anyhow::anyhow!(err))?;
        Ok(value.to_string())
    })
}

/// Scalar body expansion: no IFS splitting, pathname, brace, or tilde work.
///
/// Mirrors `expand_operand_scalar` but also evaluates nested arithmetic.
/// Eager by design (see `expand_arithmetic_to_decimal`): nested expansions
/// run before the outer arithmetic parse, so outer `&&` / `||` / `?:`
/// short-circuit cannot suppress them. The caller parses the joined string.
fn expand_body_scalar<'a>(
    shell: &'a mut Shell,
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
                super::plan::WordPart::Literal(literal) => {
                    let seg = literal_segment(literal, shell, first_part, context);
                    out.push_str(&seg.text);
                }
                super::plan::WordPart::Variable { source, quote } => {
                    let seg = variable_segment(source, *quote, shell, context);
                    out.push_str(&seg.text);
                }
                super::plan::WordPart::ParameterExpansion { expansion, quote } => {
                    let scalar = super::word_expand_param::expand_parameter_to_scalar(
                        shell, ctx, expansion, *quote, confirm, resources, trace,
                    )
                    .await?;
                    out.push_str(&scalar);
                }
                super::plan::WordPart::ArithmeticExpansion { expansion, .. } => {
                    let nested = expand_arithmetic_to_decimal(
                        shell, ctx, expansion, confirm, resources, trace,
                    )
                    .await?;
                    out.push_str(&nested);
                }
                super::plan::WordPart::Substitution { substitution, .. } => match substitution.kind
                {
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

/// Live argument/operand splicing for `expand_word_segments`.
///
/// Wraps the decimal result so unquoted arithmetic participates in the
/// existing IFS pipeline while quoted stays protected.
#[allow(clippy::too_many_arguments)]
pub(crate) fn expand_arithmetic_to_segments<'a>(
    shell: &'a mut Shell,
    ctx: &'a Context,
    expansion: &'a PlannedArithmeticExpansion,
    quote: QuoteMode,
    confirm: ConfirmFn,
    resources: &'a mut ExecutionResources,
    trace: &'a mut ExpansionTrace,
    context: ExpansionContext,
) -> Pin<Box<dyn Future<Output = Result<Vec<ExpandedSegment>>> + 'a>> {
    Box::pin(async move {
        let decimal =
            expand_arithmetic_to_decimal(shell, ctx, expansion, confirm, resources, trace).await?;
        let quoted = quote != QuoteMode::Unquoted;
        Ok(vec![dynamic_text_segment(decimal, quoted, context)])
    })
}

/// Dry placeholder for safety preflight: never expands, evaluates, spawns,
/// or mutates. Mirrors command-substitution placeholders.
pub(crate) fn dry_arithmetic_placeholder(expansion: &PlannedArithmeticExpansion) -> String {
    format!("$(({}))", expansion.source)
}

pub(crate) fn dry_arithmetic_to_segments(
    expansion: &PlannedArithmeticExpansion,
    quote: QuoteMode,
    shell: &Shell,
) -> Vec<ExpandedSegment> {
    let _ = shell;
    let placeholder = dry_arithmetic_placeholder(expansion);
    let quoted = quote != QuoteMode::Unquoted;
    // Placeholders never split or glob; they only shape the dry argv.
    vec![ExpandedSegment::protected(
        placeholder.clone(),
        escape_glob_metacharacters(&placeholder),
        PatternKind::Inactive,
        quoted || !placeholder.is_empty(),
    )]
}

pub(crate) fn dry_arithmetic_to_scalar(
    expansion: &PlannedArithmeticExpansion,
    shell: &Shell,
) -> String {
    let _ = shell;
    dry_arithmetic_placeholder(expansion)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_placeholder_never_evaluates() {
        use crate::shell::plan::PlannedWord;
        let expansion = PlannedArithmeticExpansion {
            source: "X = 1".to_string(),
            body: Box::new(PlannedWord {
                source: "X = 1".to_string(),
                parts: vec![],
            }),
        };
        assert_eq!(dry_arithmetic_placeholder(&expansion), "$((X = 1))");
        // Dry scalar keeps the placeholder, never `1`.
        let env = crate::environment::Environment::new();
        let shell = Shell::new(env);
        assert_eq!(dry_arithmetic_to_scalar(&expansion, &shell), "$((X = 1))");
    }

    /// Dry preflight never evaluates, assigns, or spawns: the projection is
    /// a protected placeholder and the shell is untouched.
    #[test]
    fn dry_arithmetic_has_no_side_effects() {
        use super::super::word_expand::{dry_expand_argument_word, dry_expand_scalar_word};
        use std::sync::Arc;

        let env = crate::environment::Environment::new();
        let shell = Shell::new(env);
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        {
            let mut guard = shell.environment.write();
            guard.set_shell_var("DOGESH_DRY_ARITH".to_string(), "1".to_string());
            guard.unset_shell_var("DOGESH_DRY_ARITH_ASSIGN");
        }
        let parse_word_for = |input: &str| {
            let plan =
                super::super::parse::parse_execution_plan(input, Arc::clone(&shell.environment))
                    .expect("plan");
            plan.lists[0].jobs[0].stages[0].argv[1].clone()
        };
        // Assignment stays placeholder, never stored.
        let word = parse_word_for("echo $((DOGESH_DRY_ARITH_ASSIGN = 1))");
        let fields = dry_expand_argument_word(&word, &shell, &cwd);
        assert!(
            fields
                .iter()
                .any(|f| f.contains("$((DOGESH_DRY_ARITH_ASSIGN = 1))"))
        );
        assert!(
            shell
                .environment
                .read()
                .lookup_variable("DOGESH_DRY_ARITH_ASSIGN")
                .is_none(),
            "dry arithmetic must not assign"
        );
        let word = parse_word_for("echo $((DOGESH_DRY_ARITH_ASSIGN = 1))");
        assert_eq!(
            dry_expand_scalar_word(&word, &shell),
            "$((DOGESH_DRY_ARITH_ASSIGN = 1))".to_string()
        );
        // Nested command stays placeholder, marker absent.
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("dry_arith_must_not_run");
        let input = format!("echo $(( $(touch {}) + 1 ))", marker.display());
        let word = parse_word_for(&input);
        let fields = dry_expand_argument_word(&word, &shell, &cwd);
        assert!(fields.iter().any(|f| f.contains("touch")));
        assert!(!marker.exists(), "dry arithmetic must not spawn");
    }

    /// Selected arithmetic command substitution goes through the existing
    /// authorize-before-run path and is denyable; nothing runs on denial.
    #[tokio::test]
    async fn selected_arithmetic_substitution_is_denyable() {
        use super::super::materialize::{MaterializeOutcome, materialize_job};
        use crate::repl::confirmation::ConfirmationAction;
        use std::sync::Arc;

        fn deny_all(_: &str) -> anyhow::Result<ConfirmationAction> {
            Ok(ConfirmationAction::No)
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let victim = dir.path().join("victim");
        std::fs::create_dir(&victim).expect("victim");
        let input = format!("echo $(( $(rm -rf {}) + 1 ))", victim.display());
        let env = crate::environment::Environment::new();
        let mut shell = Shell::new(env);
        let plan =
            super::super::parse::parse_execution_plan(&input, Arc::clone(&shell.environment))
                .expect("plan");
        let ctx = dsh_types::Context::new_safe(shell.pid, shell.pgid, false);
        match materialize_job(&mut shell, &ctx, &plan.lists[0].jobs[0], deny_all).await {
            Err(err) => assert!(
                super::super::authorize::is_authorization_cancelled(&err),
                "denied arithmetic body must surface as cancellation, got {err:?}"
            ),
            Ok(_) => panic!("denied arithmetic body must not materialize"),
        }
        assert!(victim.exists(), "denied arithmetic rm must not run");
        let _ = MaterializeOutcome::Runnable;
    }
}
