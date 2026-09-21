//! Parser coverage for asynchronous AND-OR lists.
//!
//! Lives in `dsh/tests/` rather than as unit tests in `dsh/src/shell/parse.rs`
//! so the new grammar cases do not push that file over the 800-line
//! file-budget limit.

use doge_shell::environment::Environment;
use doge_shell::process::ListOp;
use doge_shell::shell::parse::parse_execution_plan;
use doge_shell::shell::plan::ListExecutionMode;
use std::sync::Arc;

/// `&` backgrounds the whole AND-OR list, not one command.
#[test]
fn async_list_plan_shapes() {
    let env = Environment::new();
    let plan = parse_execution_plan("echo x &", Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists.len(), 1);
    assert_eq!(plan.lists[0].execution, ListExecutionMode::Asynchronous);
    assert_eq!(plan.lists[0].jobs.len(), 1);

    let plan = parse_execution_plan("echo x & echo y", Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists.len(), 2);
    assert_eq!(plan.lists[0].execution, ListExecutionMode::Asynchronous);
    assert_eq!(plan.lists[1].execution, ListExecutionMode::Foreground);

    let plan = parse_execution_plan("echo x | cat &", Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists.len(), 1);
    assert_eq!(plan.lists[0].execution, ListExecutionMode::Asynchronous);
    assert_eq!(plan.lists[0].jobs.len(), 1);
    assert_eq!(plan.lists[0].jobs[0].stages.len(), 2);

    let plan = parse_execution_plan("echo x | cat & echo y", Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists.len(), 2);
    assert_eq!(plan.lists[0].execution, ListExecutionMode::Asynchronous);
    assert_eq!(plan.lists[0].jobs[0].stages.len(), 2);
    assert_eq!(plan.lists[1].execution, ListExecutionMode::Foreground);

    let plan = parse_execution_plan("true && echo yes &", Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists.len(), 1);
    assert_eq!(plan.lists[0].execution, ListExecutionMode::Asynchronous);
    assert_eq!(plan.lists[0].jobs.len(), 2);
    assert_eq!(plan.lists[0].jobs[0].list_op, ListOp::And);
    assert_eq!(plan.lists[0].jobs[1].list_op, ListOp::None);

    let plan = parse_execution_plan("false || echo yes &", Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists.len(), 1);
    assert_eq!(plan.lists[0].execution, ListExecutionMode::Asynchronous);
    assert_eq!(plan.lists[0].jobs[0].list_op, ListOp::Or);

    // `a && b || c & d`: the async body is the whole `a && b || c`,
    // `d` is the next foreground list.
    let plan = parse_execution_plan("a && b || c & d", Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists.len(), 2);
    assert_eq!(plan.lists[0].execution, ListExecutionMode::Asynchronous);
    assert_eq!(plan.lists[0].jobs.len(), 3);
    assert_eq!(plan.lists[0].jobs[0].list_op, ListOp::And);
    assert_eq!(plan.lists[0].jobs[1].list_op, ListOp::Or);
    assert_eq!(plan.lists[0].jobs[2].list_op, ListOp::None);
    assert_eq!(plan.lists[1].execution, ListExecutionMode::Foreground);
    assert_eq!(plan.lists[1].jobs.len(), 1);

    // A trailing `;` stays valid and foreground.
    let plan = parse_execution_plan("echo hi;", Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists.len(), 1);
    assert_eq!(plan.lists[0].execution, ListExecutionMode::Foreground);
}

/// Doubled separators and leading `&` are strict syntax errors.
#[test]
fn invalid_list_separators_are_rejected() {
    let env = Environment::new();
    for input in [
        "& echo a",
        "echo a & & echo b",
        "echo a && & echo b",
        "echo a &; echo b",
    ] {
        let err = parse_execution_plan(input, Arc::clone(&env)).expect_err("must reject {input:?}");
        assert!(
            err.to_string().contains("syntax error"),
            "unexpected error for {input:?}: {err:?}"
        );
    }
}
