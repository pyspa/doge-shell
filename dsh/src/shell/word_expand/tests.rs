use super::segments::apply_tilde;
use super::*;
use crate::repl::confirmation::ConfirmationAction;
use crate::shell::Shell;
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
#[test]
fn dry_invocation_expansion_uses_real_values_and_field_count() {
    let env = crate::environment::Environment::new();
    env.write().invocation = crate::environment::InvocationParameters {
        interactive: false,
        argv0: "worker".into(),
        positional: vec!["important-file".into(), "".into(), "*.rs".into()],
    };
    let shell = Shell::new(env.clone());
    let plan =
        super::super::parse::parse_execution_plan("rm \"$1\" \"$@\" \"pre$@post\"", env).unwrap();
    let argv = &plan.lists[0].jobs[0].stages[0].argv;
    assert_eq!(
        dry_expand_argument_word(&argv[1], &shell, &std::env::current_dir().unwrap()),
        ["important-file"]
    );
    assert_eq!(
        dry_expand_argument_word(&argv[2], &shell, &std::env::current_dir().unwrap()),
        ["important-file", "", "*.rs"]
    );
    assert_eq!(
        dry_expand_argument_word(&argv[3], &shell, &std::env::current_dir().unwrap()),
        ["preimportant-file", "", "*.rspost"]
    );
}
