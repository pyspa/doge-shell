//! Runtime word expansion for selected jobs.
//!
//! Planning preserves one source word as structured parts. This module reads
//! current shell state at materialization time (variables, `$?`, `HOME`, cwd)
//! and builds concrete fields. Alias rewriting is syntax-time only and never
//! happens here.

use super::authorize::ConfirmFn;
use super::plan::{PlannedLiteral, PlannedWord, QuoteMode, WordPart};
use super::substitution::{
    ExecutionResources, capture_subshell_plan_stdout, start_process_substitution,
};
use crate::parser::expansion::{
    escape_glob_metacharacters, expand_braces, expand_glob_pattern, unescape_glob_metacharacters,
};
use crate::process::SubshellType;
use crate::process::reexec::PlanExecMode;
use crate::shell::Shell;
use anyhow::{Result, bail};
use dsh_types::Context;
use std::path::PathBuf;

#[derive(Debug, Clone)]
struct ExpandedField {
    text: String,
    pattern: String,
    has_active_pattern: bool,
    has_brace: bool,
    preserve_empty: bool,
}

#[derive(Debug, Default)]
struct FieldBuilder {
    fields: Vec<ExpandedField>,
}

impl FieldBuilder {
    fn new() -> Self {
        Self {
            fields: vec![ExpandedField {
                text: String::new(),
                pattern: String::new(),
                has_active_pattern: false,
                has_brace: false,
                preserve_empty: false,
            }],
        }
    }

    fn current_mut(&mut self) -> &mut ExpandedField {
        if self.fields.is_empty() {
            self.fields.push(ExpandedField {
                text: String::new(),
                pattern: String::new(),
                has_active_pattern: false,
                has_brace: false,
                preserve_empty: false,
            });
        }
        self.fields.last_mut().expect("field")
    }

    fn append_single(
        &mut self,
        text: &str,
        pattern: &str,
        active: bool,
        brace: bool,
        preserve: bool,
    ) {
        let current = self.current_mut();
        current.text.push_str(text);
        current.pattern.push_str(pattern);
        current.has_active_pattern |= active;
        current.has_brace |= brace;
        current.preserve_empty |= preserve;
    }

    fn append_multi(&mut self, fragments: Vec<(String, String)>) {
        if fragments.is_empty() {
            return;
        }
        let mut iter = fragments.into_iter();
        let (first_text, first_pattern) = iter.next().expect("non-empty");
        {
            let current = self.current_mut();
            current.text.push_str(&first_text);
            current.pattern.push_str(&first_pattern);
        }
        for (text, pattern) in iter {
            self.fields.push(ExpandedField {
                text,
                pattern,
                has_active_pattern: false,
                has_brace: false,
                preserve_empty: false,
            });
        }
    }

    fn finish_argument(self, cwd: &std::path::Path) -> Vec<String> {
        let mut out = Vec::new();
        for field in self.fields {
            if !field.has_active_pattern && !field.has_brace {
                if field.text.is_empty() && !field.preserve_empty {
                    continue;
                }
                out.push(field.text);
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

    fn finish_scalar(self) -> String {
        let mut out = String::new();
        for field in self.fields {
            out.push_str(&field.text);
        }
        out
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

fn resolve_variable(source: &str, shell: &Shell) -> String {
    shell
        .environment
        .read()
        .get_var(source)
        .unwrap_or_else(|| source.to_string())
}

fn literal_fragment(
    text: &str,
    raw: &str,
    quote: QuoteMode,
    pattern_active: bool,
    brace_active: bool,
) -> (String, String, bool, bool, bool) {
    match quote {
        QuoteMode::Unquoted if pattern_active || brace_active => (
            text.to_string(),
            raw.to_string(),
            pattern_active,
            brace_active,
            false,
        ),
        _ => (
            text.to_string(),
            escape_glob_metacharacters(text),
            false,
            false,
            !text.is_empty() || quote != QuoteMode::Unquoted,
        ),
    }
}

fn cwd_for_expansion() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Push one static-text part, applying a leading tilde and deciding whether
/// its metacharacters stay live for glob/brace matching.
///
/// Shared by the live and dry paths so the two cannot drift apart.
fn push_literal_fragment(
    builder: &mut FieldBuilder,
    literal: &PlannedLiteral,
    shell: &Shell,
    first: bool,
) {
    let (mut text, mut pattern) = (literal.text.clone(), literal.raw.clone());
    if first && literal.tilde_candidate {
        (text, pattern) = apply_tilde(&text, &pattern, shell);
    }
    let (text, pattern, active, brace, preserve) = literal_fragment(
        &text,
        &pattern,
        literal.quote,
        literal.pattern_active,
        literal.brace_active,
    );
    builder.append_single(&text, &pattern, active, brace, preserve);
}

/// Push one variable part as a single protected fragment.
///
/// An empty value still keeps its field (the pre-migration engine re-parsed
/// it as `''`): unquoted `$EMPTY` is one empty argv, not zero fields.
fn push_variable_fragment(builder: &mut FieldBuilder, source: &str, shell: &Shell) {
    let value = resolve_variable(source, shell);
    builder.append_single(
        &value,
        &escape_glob_metacharacters(&value),
        false,
        false,
        true,
    );
}

fn trim_substitution_output(output: &str) -> String {
    output.trim_end_matches(['\n', '\r']).to_string()
}

/// Expand one word into zero, one, or many argument fields.
pub async fn expand_argument_word(
    shell: &mut Shell,
    ctx: &Context,
    word: &PlannedWord,
    confirm: ConfirmFn,
    resources: &mut ExecutionResources,
) -> Result<Vec<String>> {
    let cwd = cwd_for_expansion();
    let mut builder = FieldBuilder::new();
    let mut first_part = true;
    for part in &word.parts {
        match part {
            WordPart::Literal(literal) => {
                push_literal_fragment(&mut builder, literal, shell, first_part);
            }
            WordPart::Variable { source, .. } => {
                push_variable_fragment(&mut builder, source, shell);
            }
            WordPart::Substitution {
                substitution,
                quote,
            } => {
                let quoted = *quote != QuoteMode::Unquoted;
                match substitution.kind {
                    SubshellType::CommandSubstitution => {
                        let output = capture_subshell_plan_stdout(
                            shell,
                            ctx,
                            &substitution.plan,
                            PlanExecMode::CommandSubstitution,
                            confirm,
                        )
                        .await?;
                        if quoted {
                            let value = trim_substitution_output(&output);
                            builder.append_single(
                                &value,
                                &escape_glob_metacharacters(&value),
                                false,
                                false,
                                true,
                            );
                        } else {
                            let fragments: Vec<(String, String)> = output
                                .split_whitespace()
                                .map(|fragment| {
                                    (fragment.to_string(), escape_glob_metacharacters(fragment))
                                })
                                .collect();
                            builder.append_multi(fragments);
                        }
                    }
                    SubshellType::Subshell => {
                        let output = capture_subshell_plan_stdout(
                            shell,
                            ctx,
                            &substitution.plan,
                            PlanExecMode::Subshell,
                            confirm,
                        )
                        .await?;
                        if quoted {
                            let value = trim_substitution_output(&output);
                            builder.append_single(
                                &value,
                                &escape_glob_metacharacters(&value),
                                false,
                                false,
                                true,
                            );
                        } else {
                            let fragments: Vec<(String, String)> = output
                                .lines()
                                .map(|line| (line.to_string(), escape_glob_metacharacters(line)))
                                .collect();
                            builder.append_multi(fragments);
                        }
                    }
                    SubshellType::ProcessSubstitution => {
                        let substitution =
                            start_process_substitution(shell, ctx, &substitution.plan, confirm)
                                .await?;
                        let path = resources.add_process_substitution(substitution);
                        builder.append_single(
                            &path,
                            &escape_glob_metacharacters(&path),
                            false,
                            false,
                            true,
                        );
                    }
                    SubshellType::None => {}
                }
            }
        }
        first_part = false;
    }
    // A word with no parts (empty assignment value aside) expands to nothing;
    // an empty quoted word keeps one empty field via `preserve_empty`.
    if word.parts.is_empty() {
        return Ok(Vec::new());
    }
    Ok(builder.finish_argument(&cwd))
}

/// Expand an assignment value into exactly one string: no splitting, no glob.
pub async fn expand_assignment_value(
    shell: &mut Shell,
    ctx: &Context,
    word: &PlannedWord,
    confirm: ConfirmFn,
    resources: &mut ExecutionResources,
) -> Result<String> {
    expand_scalar_word(shell, ctx, word, confirm, resources).await
}

/// Expand a redirect target into exactly one path.
pub async fn expand_redirect_target(
    shell: &mut Shell,
    ctx: &Context,
    word: &PlannedWord,
    confirm: ConfirmFn,
    resources: &mut ExecutionResources,
) -> Result<String> {
    let fields = expand_argument_word(shell, ctx, word, confirm, resources).await?;
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
) -> Result<String> {
    let mut builder = FieldBuilder::new();
    let mut first_part = true;
    for part in &word.parts {
        match part {
            WordPart::Literal(literal) => {
                let (mut text, _) = (literal.text.clone(), literal.raw.clone());
                if first_part && literal.tilde_candidate {
                    (text, _) = apply_tilde(&text, &text, shell);
                }
                builder.append_single(&text, "", false, false, true);
            }
            WordPart::Variable { source, .. } => {
                let value = resolve_variable(source, shell);
                builder.append_single(&value, "", false, false, true);
            }
            WordPart::Substitution { substitution, .. } => match substitution.kind {
                SubshellType::CommandSubstitution | SubshellType::Subshell => {
                    let mode = match substitution.kind {
                        SubshellType::Subshell => PlanExecMode::Subshell,
                        _ => PlanExecMode::CommandSubstitution,
                    };
                    let output =
                        capture_subshell_plan_stdout(shell, ctx, &substitution.plan, mode, confirm)
                            .await?;
                    // Scalar context never splits; keep newlines except trailing.
                    let value = trim_substitution_output(&output);
                    builder.append_single(&value, "", false, false, true);
                }
                SubshellType::ProcessSubstitution => {
                    let substitution =
                        start_process_substitution(shell, ctx, &substitution.plan, confirm).await?;
                    let path = resources.add_process_substitution(substitution);
                    builder.append_single(&path, "", false, false, true);
                }
                SubshellType::None => {}
            },
        }
        first_part = false;
    }
    if word.parts.is_empty() {
        return Ok(String::new());
    }
    Ok(builder.finish_scalar())
}

/// Read-only expansion for safety preflight: no spawn, no env mutation.
///
/// Mirrors [`expand_argument_word`] through the shared `push_*_fragment`
/// helpers; only substitution handling differs (diagnostic placeholder).
pub fn dry_expand_argument_word(
    word: &PlannedWord,
    shell: &Shell,
    cwd: &std::path::Path,
) -> Vec<String> {
    let mut builder = FieldBuilder::new();
    let mut first_part = true;
    for part in &word.parts {
        match part {
            WordPart::Literal(literal) => {
                push_literal_fragment(&mut builder, literal, shell, first_part);
            }
            WordPart::Variable { source, .. } => {
                push_variable_fragment(&mut builder, source, shell);
            }
            WordPart::Substitution {
                substitution,
                quote,
            } => {
                let placeholder = match substitution.kind {
                    SubshellType::CommandSubstitution => format!("$({})", substitution.source),
                    SubshellType::ProcessSubstitution => format!("<({})", substitution.source),
                    SubshellType::Subshell => format!("({})", substitution.source),
                    SubshellType::None => String::new(),
                };
                let quoted = *quote != QuoteMode::Unquoted;
                builder.append_single(
                    &placeholder,
                    &escape_glob_metacharacters(&placeholder),
                    false,
                    false,
                    quoted || !placeholder.is_empty(),
                );
            }
        }
        first_part = false;
    }
    if word.parts.is_empty() {
        return Vec::new();
    }
    builder.finish_argument(cwd)
}

/// Read-only scalar expansion for safety preflight.
pub fn dry_expand_scalar_word(word: &PlannedWord, shell: &Shell) -> String {
    let mut out = String::new();
    let mut first_part = true;
    for part in &word.parts {
        match part {
            WordPart::Literal(literal) => {
                let mut text = literal.text.clone();
                if first_part && literal.tilde_candidate {
                    (text, _) = apply_tilde(&text, &text, shell);
                }
                out.push_str(&text);
            }
            WordPart::Variable { source, .. } => {
                out.push_str(&resolve_variable(source, shell));
            }
            WordPart::Substitution { substitution, .. } => match substitution.kind {
                SubshellType::CommandSubstitution => {
                    out.push_str(&format!("$({})", substitution.source));
                }
                SubshellType::ProcessSubstitution => {
                    out.push_str(&format!("<({})", substitution.source));
                }
                SubshellType::Subshell => {
                    out.push_str(&format!("({})", substitution.source));
                }
                SubshellType::None => {}
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

    /// An unquoted empty variable keeps one empty field: the pre-migration
    /// engine re-parsed it as `''`, so `$EMPTY` was one empty argv, not zero
    /// fields.
    #[tokio::test]
    async fn unquoted_empty_variable_keeps_one_empty_field() {
        let mut shell = shell_with_empty_var();
        let ctx = Context::new_safe(shell.pid, shell.pgid, false);
        let plan = super::super::parse::parse_execution_plan(
            "echo $DOGESH_EMPTY_PROBE",
            Arc::clone(&shell.environment),
        )
        .expect("plan");
        let word = &plan.jobs[0].stages[0].argv[1];
        let mut resources = ExecutionResources::new();
        let fields = expand_argument_word(&mut shell, &ctx, word, allow_all, &mut resources)
            .await
            .expect("expand");
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
