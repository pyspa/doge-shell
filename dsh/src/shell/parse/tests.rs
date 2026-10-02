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

fn single_parameter_expansion(
    input: &str,
) -> (
    String,
    super::super::plan::ParameterCondition,
    super::super::plan::ParameterAction,
    super::super::plan::QuoteMode,
) {
    use super::super::plan::WordPart;

    let env = test_env();
    let plan = parse_execution_plan(input, Arc::clone(&env)).expect("plan");
    let word = &plan.lists[0].jobs[0].stages[0].argv[1];
    assert_eq!(word.parts.len(), 1, "for {input:?}: {word:?}");
    match &word.parts[0] {
        WordPart::ParameterExpansion { expansion, quote } => (
            expansion.name.clone(),
            expansion.condition,
            expansion.action,
            *quote,
        ),
        other => panic!("for {input:?}: expected ParameterExpansion, got {other:?}"),
    }
}

/// All eight `${VAR-op}` forms parse to their two semantic axes.
#[test]
fn all_eight_forms_parse_to_condition_and_action() {
    use super::super::plan::{ParameterAction, ParameterCondition, QuoteMode};

    for (input, cond, act) in [
        (
            "echo ${A:-fallback}",
            ParameterCondition::UnsetOrNull,
            ParameterAction::Default,
        ),
        (
            "echo ${A-fallback}",
            ParameterCondition::UnsetOnly,
            ParameterAction::Default,
        ),
        (
            "echo ${A:=fallback}",
            ParameterCondition::UnsetOrNull,
            ParameterAction::Assign,
        ),
        (
            "echo ${A=fallback}",
            ParameterCondition::UnsetOnly,
            ParameterAction::Assign,
        ),
        (
            "echo ${A:?message}",
            ParameterCondition::UnsetOrNull,
            ParameterAction::Error,
        ),
        (
            "echo ${A?message}",
            ParameterCondition::UnsetOnly,
            ParameterAction::Error,
        ),
        (
            "echo ${A:+alternative}",
            ParameterCondition::UnsetOrNull,
            ParameterAction::Alternate,
        ),
        (
            "echo ${A+alternative}",
            ParameterCondition::UnsetOnly,
            ParameterAction::Alternate,
        ),
    ] {
        let (name, c, a, q) = single_parameter_expansion(input);
        assert_eq!(name, "A", "for {input:?}");
        assert_eq!(c, cond, "for {input:?}");
        assert_eq!(a, act, "for {input:?}");
        assert_eq!(q, QuoteMode::Unquoted, "for {input:?}");
    }
}

/// `"${A:-fallback}"` carries outer `Double` quote.
#[test]
fn double_quoted_outer_carries_double_quote() {
    use super::super::plan::QuoteMode;

    let (_, _, _, quote) = single_parameter_expansion("echo \"${A:-fallback}\"");
    assert_eq!(quote, QuoteMode::Double);
}

/// `${A:-${B:-fallback}}` nests structurally.
#[test]
fn nested_parameter_expansion_stays_structured() {
    use super::super::plan::WordPart;

    let env = test_env();
    let plan = parse_execution_plan("echo ${A:-${B:-fallback}}", Arc::clone(&env)).expect("plan");
    let word = &plan.lists[0].jobs[0].stages[0].argv[1];
    let WordPart::ParameterExpansion { expansion, .. } = &word.parts[0] else {
        panic!("outer missing: {word:?}");
    };
    assert_eq!(expansion.name, "A");
    let operand = expansion.word.as_deref().expect("operand");
    assert_eq!(operand.parts.len(), 1);
    match &operand.parts[0] {
        WordPart::ParameterExpansion { expansion, .. } => {
            assert_eq!(expansion.name, "B");
            assert!(expansion.word.is_some());
        }
        other => panic!("nested missing: {other:?}"),
    }
    assert!(plan.lists[0].jobs[0].contains_dynamic_expansion());
}

/// Planning `${A:-$(touch marker)}` records but never runs the body.
#[test]
fn parameter_operand_substitution_is_side_effect_free() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("param_parse_must_not_run");
    let input = format!("echo ${{A:-$(touch {})}}", marker.display());
    let env = test_env();
    let plan = parse_execution_plan(&input, Arc::clone(&env)).expect("plan");
    assert!(plan.lists[0].jobs[0].contains_dynamic_expansion());
    assert!(
        !marker.exists(),
        "planning executed an operand it must only record"
    );
}

/// Matching `}` respects nesting, quotes, escapes, and substitutions.
#[test]
fn matching_brace_is_structural() {
    let env = test_env();
    for input in [
        "echo ${A:-${B:-x}}",
        "echo ${A:-\"a}b\"}",
        "echo ${A:-'a}b'}",
        "echo ${A:-a\\}b}",
        "echo ${A:-$(printf '}')}",
    ] {
        let plan = parse_execution_plan(input, Arc::clone(&env)).expect("plan {input:?}");
        assert_eq!(plan.lists.len(), 1, "for {input:?}");
    }
    let err = parse_execution_plan("echo ${A:-unterminated", Arc::clone(&test_env()))
        .expect_err("unterminated must fail");
    assert!(err.to_string().contains("syntax error"), "got {err:?}");
}

/// Unsupported `${...}` forms fail strict parsing instead of running truncated.
#[test]
fn unsupported_modified_forms_fail_cleanly() {
    for input in [
        "echo ${#X}",
        "echo ${X%foo}",
        "echo ${X%%foo}",
        "echo ${X#foo}",
        "echo ${X##foo}",
    ] {
        let err = parse_execution_plan(input, Arc::clone(&test_env()))
            .expect_err("{input:?} must not parse");
        assert!(
            err.to_string().contains("syntax error"),
            "for {input:?}: got {err:?}"
        );
    }
}

#[test]
fn invocation_parameters_keep_identity_and_single_digit_unbraced_positions() {
    use crate::shell::plan::InvocationParameter::*;
    for (source, expected) in [
        ("$0", Arg0),
        ("$1", Positional(1)),
        ("$9", Positional(9)),
        ("${0}", Arg0),
        ("${1}", Positional(1)),
        ("${10}", Positional(10)),
        ("${123}", Positional(123)),
        ("$#", Count),
        ("${#}", Count),
        ("$@", At),
        ("${@}", At),
        ("$*", Star),
        ("${*}", Star),
    ] {
        for quoted in [false, true] {
            let input = if quoted {
                format!("echo \"{source}\"")
            } else {
                format!("echo {source}")
            };
            let plan = parse_execution_plan(&input, test_env()).unwrap();
            assert_eq!(
                plan.lists[0].jobs[0].stages[0].argv[1].parts,
                vec![WordPart::InvocationParameter {
                    parameter: expected,
                    quote: if quoted {
                        QuoteMode::Double
                    } else {
                        QuoteMode::Unquoted
                    }
                }]
            );
        }
    }
    let plan = parse_execution_plan("echo $10 ${10} $$ $! $?", test_env()).unwrap();
    let argv = &plan.lists[0].jobs[0].stages[0].argv;
    assert!(matches!(argv[1].parts.as_slice(), [
        WordPart::InvocationParameter { parameter: Positional(1), .. },
        WordPart::Literal(literal)] if literal.text == "0"));
    assert_eq!(argv[2].parts.len(), 1);
    for word in &argv[3..] {
        assert!(matches!(word.parts.as_slice(), [WordPart::Variable { .. }]));
    }
}

#[test]
fn invocation_parameters_parse_in_operands_and_arithmetic() {
    for source in [
        "echo ${X:-$1}",
        "echo ${X:-$#}",
        "echo ${X:-$*}",
        "echo $(( $1 + $# ))",
    ] {
        assert!(parse_execution_plan(source, test_env()).is_ok(), "{source}");
    }
    for source in ["echo ${1:-default}", "echo ${@:+value}"] {
        assert!(
            parse_execution_plan(source, test_env()).is_err(),
            "{source}"
        );
    }
}
