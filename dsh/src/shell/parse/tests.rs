use super::*;
use crate::environment::Environment;

fn test_env() -> Arc<RwLock<Environment>> {
    Environment::new()
}

/// Test A: planning alone must not execute substitutions.
#[test]
fn planning_does_not_execute_substitution() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("parse_must_not_run");
    let input = format!("echo $(touch {})", marker.display());
    let env = test_env();
    let cwd = std::env::current_dir().expect("cwd");
    let vars_before = {
        let guard = env.read();
        guard.variable_state.variables.clone()
    };
    let plan = parse_execution_plan(&input, Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists.len(), 1);
    assert!(plan.lists[0].jobs[0].contains_dynamic_expansion());
    assert!(
        !marker.exists(),
        "planning executed a substitution it must only record"
    );
    assert_eq!(
        std::env::current_dir().expect("cwd"),
        cwd,
        "planning must not change directories"
    );
    let vars_after = env.read().variable_state.variables.clone();
    assert_eq!(
        vars_before, vars_after,
        "planning must not mutate variables"
    );
}

/// Test B: a standalone assignment is deferred, not applied by planning.
#[test]
fn planning_does_not_apply_standalone_assignment() {
    let env = test_env();
    let plan =
        parse_execution_plan("DOGESH_TEST_PARSE_ONLY=value", Arc::clone(&env)).expect("plan");
    assert_eq!(plan.lists.len(), 1);
    assert!(env.read().get_var("DOGESH_TEST_PARSE_ONLY").is_none());
    assert!(plan.lists[0].jobs[0].is_assignment_only());
}

/// Raw validation runs before rewriting: `echo $FOO )` must still reject
/// the line instead of being discarded when the prefix is re-serialized.
#[test]
fn raw_tail_is_rejected_before_expansion_can_discard_it() {
    let env = test_env();
    env.write()
        .variable_state
        .variables
        .insert("$FOO".to_string(), "bar".to_string());
    let err = parse_execution_plan("echo $FOO )", Arc::clone(&env))
        .expect_err("raw tail must be a syntax error");
    assert!(
        err.to_string().contains("syntax error"),
        "unexpected error: {err:?}"
    );
}

/// Post-rewrite validation: a raw-complete line whose alias expands to
/// invalid syntax must not produce a plan.
#[test]
fn expanded_tail_is_rejected_after_expansion() {
    let env = test_env();
    env.write()
        .variable_state
        .alias
        .insert("bad".to_string(), "echo expanded )".to_string());
    let err = parse_execution_plan("bad", Arc::clone(&env))
        .expect_err("expanded tail must be a syntax error");
    assert!(
        err.to_string().contains("syntax error"),
        "unexpected error: {err:?}"
    );
}

/// Boundary pin: `Rule::commands` stays tolerant for REPL highlighting and
/// completion, while execution is strict. The editor sees the partial
/// prefix plus an `unparsed_tail`; the planner returns an error.
#[test]
fn tolerant_grammar_and_strict_execution_stay_separate() {
    use pest::Parser as _;

    let input = "echo a )";
    let pairs = ShellParser::parse(Rule::commands, input).expect("tolerant parse");
    let consumed = pairs
        .clone()
        .next()
        .map(|pair| pair.as_span().end())
        .unwrap_or(0);
    assert_eq!(parser::unparsed_tail(input, consumed), Some(")"));

    let env = test_env();
    let err = parse_execution_plan(input, Arc::clone(&env)).expect_err("execution must be strict");
    assert!(
        err.to_string().contains("syntax error"),
        "unexpected error: {err:?}"
    );
}

/// Planning is environment-independent except for aliases: the same word
/// structure comes back regardless of variable values or cwd.
#[test]
fn planning_preserves_word_structure_across_environments() {
    use super::super::plan::QuoteMode;

    let env_a = test_env();
    env_a
        .write()
        .set_shell_var("FOO".to_string(), "aaa".to_string());
    let env_b = test_env();
    env_b
        .write()
        .set_shell_var("FOO".to_string(), "bbb".to_string());
    let plan_a = parse_execution_plan("echo $FOO *.txt", Arc::clone(&env_a)).expect("plan");
    let plan_b = parse_execution_plan("echo $FOO *.txt", Arc::clone(&env_b)).expect("plan");
    assert_eq!(plan_a.lists.len(), 1);
    assert_eq!(plan_b.lists.len(), 1);
    let argv_a = &plan_a.lists[0].jobs[0].stages[0].argv;
    let argv_b = &plan_b.lists[0].jobs[0].stages[0].argv;
    assert_eq!(argv_a.len(), argv_b.len());
    assert!(matches!(
        argv_a[1].parts[0],
        WordPart::Variable {
            quote: QuoteMode::Unquoted,
            ..
        }
    ));
    assert!(plan_a.lists[0].jobs[0].contains_dynamic_expansion());
    assert!(plan_b.lists[0].jobs[0].contains_dynamic_expansion());
}

fn substitution_kinds(plan: &ExecutionPlan) -> Vec<PlannedSubstitutionKind> {
    let mut kinds = Vec::new();
    for job in plan.iter_jobs() {
        for stage in &job.stages {
            for word in stage
                .argv
                .iter()
                .chain(
                    stage
                        .redirects
                        .iter()
                        .filter_map(|redirect| match &redirect.op {
                            PlannedRedirectOp::ReadFile(word)
                            | PlannedRedirectOp::WriteFile(word)
                            | PlannedRedirectOp::AppendFile(word)
                            | PlannedRedirectOp::BothWrite(word)
                            | PlannedRedirectOp::BothAppend(word) => Some(word),
                            PlannedRedirectOp::DupFrom(_) | PlannedRedirectOp::Close => None,
                        }),
                )
            {
                for part in &word.parts {
                    if let WordPart::Substitution { substitution, .. } = part {
                        kinds.push(substitution.kind.clone());
                    }
                }
            }
        }
    }
    kinds
}

/// `<(...)` parses as `Process(Read)` (regression pin).
#[test]
fn input_substitution_parses_as_read() {
    use super::super::plan::{PlannedSubstitutionKind, ProcessSubstitutionDirection};

    let env = test_env();
    let plan = parse_execution_plan("cat <(printf x)", Arc::clone(&env)).expect("plan");
    let kinds = substitution_kinds(&plan);
    assert_eq!(
        kinds,
        vec![PlannedSubstitutionKind::Process(
            ProcessSubstitutionDirection::Read
        )],
    );
}

/// `tee >(cat)` keeps `Process(Write)` in the argv word.
#[test]
fn output_argument_parses_as_write() {
    use super::super::plan::{PlannedSubstitutionKind, ProcessSubstitutionDirection};

    let env = test_env();
    let plan = parse_execution_plan("tee >(cat)", Arc::clone(&env)).expect("plan");
    let kinds = substitution_kinds(&plan);
    assert_eq!(
        kinds,
        vec![PlannedSubstitutionKind::Process(
            ProcessSubstitutionDirection::Write
        )],
    );
}

/// `printf x > >(cat)` keeps `Process(Write)` in the redirect target.
#[test]
fn output_redirect_target_parses_as_write() {
    use super::super::plan::{PlannedSubstitutionKind, ProcessSubstitutionDirection};

    let env = test_env();
    let plan = parse_execution_plan("printf x > >(cat)", Arc::clone(&env)).expect("plan");
    let kinds = substitution_kinds(&plan);
    assert_eq!(
        kinds,
        vec![PlannedSubstitutionKind::Process(
            ProcessSubstitutionDirection::Write
        )],
    );
    // The redirect itself is a plain `>` file redirect whose target word
    // carries the substitution; the direction is not a redirect op.
    let redirect = &plan.lists[0].jobs[0].stages[0].redirects[0];
    assert!(matches!(redirect.op, PlannedRedirectOp::WriteFile(_)));
}

/// Mixed directions on one line keep their own direction each.
#[test]
fn mixed_substitutions_keep_individual_directions() {
    use super::super::plan::{PlannedSubstitutionKind, ProcessSubstitutionDirection};

    let env = test_env();
    let plan = parse_execution_plan("diff <(a) <(b) > >(c)", Arc::clone(&env)).expect("plan");
    let kinds = substitution_kinds(&plan);
    assert_eq!(
        kinds,
        vec![
            PlannedSubstitutionKind::Process(ProcessSubstitutionDirection::Read),
            PlannedSubstitutionKind::Process(ProcessSubstitutionDirection::Read),
            PlannedSubstitutionKind::Process(ProcessSubstitutionDirection::Write),
        ],
    );
}

/// Nested `>(...)` survives inside `$(...)` without losing direction.
#[test]
fn nested_output_substitution_keeps_write() {
    use super::super::plan::{PlannedSubstitutionKind, ProcessSubstitutionDirection};

    let env = test_env();
    let plan = parse_execution_plan("echo $(printf x > >(cat))", Arc::clone(&env)).expect("plan");
    // Outer `$(...)` is `Command`.
    let outer = substitution_kinds(&plan);
    assert_eq!(outer, vec![PlannedSubstitutionKind::Command]);
    // Inner plan (inside `$(...)`) carries the `Write`.
    let word = &plan.lists[0].jobs[0].stages[0].argv[1];
    let Some(WordPart::Substitution { substitution, .. }) = word
        .parts
        .iter()
        .find(|part| matches!(part, WordPart::Substitution { .. }))
    else {
        panic!("outer substitution missing");
    };
    let inner = substitution_kinds(&substitution.plan);
    assert_eq!(
        inner,
        vec![PlannedSubstitutionKind::Process(
            ProcessSubstitutionDirection::Write
        )],
    );
}
