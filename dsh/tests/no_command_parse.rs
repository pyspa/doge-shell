//! Parser coverage for no-command simple commands.
//!
//! Lives in `dsh/tests/` rather than as unit tests in `dsh/src/shell/parse.rs`
//! so the new grammar cases do not push that file over the 800-line
//! file-budget limit.

use doge_shell::environment::Environment;
use doge_shell::shell::parse::parse_execution_plan;
use doge_shell::shell::plan::PlannedRedirectOp;
use std::sync::Arc;

/// Redirection-only and assignment-plus-redirect lines plan a stage with no
/// argv but concrete redirects and assignments.
#[test]
fn redirect_only_and_assignment_redirect_plan_no_command_stage() {
    let env = Environment::new();
    for (input, argv_len, redirect_len, env_len) in [
        ("> out", 0, 1, 0),
        (">> out", 0, 1, 0),
        ("< in", 0, 1, 0),
        ("2> err", 0, 1, 0),
        ("2>> err", 0, 1, 0),
        ("2>&1", 0, 1, 0),
        ("FOO=bar > out", 0, 1, 1),
        ("FOO=bar 2> err", 0, 1, 1),
    ] {
        let plan = parse_execution_plan(input, Arc::clone(&env)).expect("plan");
        assert_eq!(plan.lists.len(), 1, "for {input:?}");
        assert_eq!(plan.lists[0].jobs[0].stages.len(), 1, "for {input:?}");
        let stage = &plan.lists[0].jobs[0].stages[0];
        assert_eq!(stage.argv.len(), argv_len, "argv for {input:?}");
        assert_eq!(
            stage.redirects.len(),
            redirect_len,
            "redirects for {input:?}"
        );
        assert_eq!(stage.env_overrides.len(), env_len, "env for {input:?}");
        assert!(!stage.is_empty(), "stage must survive for {input:?}");
    }

    let plan = parse_execution_plan("> out", Arc::clone(&env)).expect("plan");
    assert!(matches!(
        plan.lists[0].jobs[0].stages[0].redirects[0].op,
        PlannedRedirectOp::WriteFile(_)
    ));
    let plan = parse_execution_plan(">> out", Arc::clone(&env)).expect("plan");
    assert!(matches!(
        plan.lists[0].jobs[0].stages[0].redirects[0].op,
        PlannedRedirectOp::AppendFile(_)
    ));
    let plan = parse_execution_plan("< in", Arc::clone(&env)).expect("plan");
    assert!(matches!(
        plan.lists[0].jobs[0].stages[0].redirects[0].op,
        PlannedRedirectOp::ReadFile(_)
    ));
    let plan = parse_execution_plan("2> err", Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists[0].jobs[0].stages[0].redirects[0].fd, 2);
    let plan = parse_execution_plan("FOO=bar > out", Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists[0].jobs[0].stages[0].env_overrides[0].name, "FOO");
}

/// A command-prefix redirect keeps both the redirect and the command:
/// `> file /bin/echo hello` runs echo with stdout to the file.
#[test]
fn command_prefix_redirect_keeps_command_and_redirect() {
    let env = Environment::new();
    let plan = parse_execution_plan("> out /bin/echo hello", Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists.len(), 1);
    assert_eq!(plan.lists[0].jobs[0].stages.len(), 1);
    let stage = &plan.lists[0].jobs[0].stages[0];
    assert_eq!(stage.argv.len(), 2);
    assert_eq!(stage.argv[0].source, "/bin/echo");
    assert_eq!(stage.redirects.len(), 1);
}
