use super::{Rule, ShellParser, rewrite_aliases, unparsed_tail};
use crate::environment::Environment;
use crate::shell::parse::parse_execution_plan;
use pest::Parser as _;
use std::sync::Arc;

#[test]
fn comments_preserve_word_parts_and_have_no_editor_command_tokens() {
    let input = "X=#value echo a#b ''#suffix \"\"#suffix \\#escaped $# ${#} # if $(bad); > file";
    let pairs = ShellParser::parse(Rule::commands, input).expect("parse");
    let consumed = pairs.clone().next().unwrap().as_span().end();
    assert_eq!(unparsed_tail(input, consumed), None);
    let comments: Vec<_> = pairs
        .clone()
        .flatten()
        .filter(|p| p.as_rule() == Rule::comment)
        .map(|p| p.as_str())
        .collect();
    assert_eq!(comments, ["# if $(bad); > file"]);
    let words = super::get_words_from_pairs(pairs.clone(), input.len());
    assert!(
        !words
            .iter()
            .any(|(_, span, _)| span.as_str().contains("bad"))
    );
    let highlights = super::collect_highlight_tokens_from_pairs(pairs, input.len());
    let comment_start = input.find("# if").unwrap();
    assert!(
        highlights
            .tokens
            .iter()
            .all(|token| token.end <= comment_start)
    );
}

#[test]
fn comment_planning_is_pure_and_does_not_record_helpers_or_redirects() {
    let env = Environment::new();
    let variables = env.read().variable_state.variables.clone();
    let plan = parse_execution_plan(
        "echo OK # > marker $(touch marker) <(touch marker) & if \" ${X:=changed}",
        Arc::clone(&env),
    )
    .expect("comment cannot cause a syntax error");
    assert_eq!(plan.lists.len(), 1);
    let job = &plan.lists[0].jobs[0];
    assert!(!job.contains_dynamic_expansion());
    assert!(job.stages[0].redirects.is_empty());
    assert_eq!(job.stages[0].argv.len(), 2);
    assert_eq!(variables, env.read().variable_state.variables);
    for input in ["# only", "# only\n", "# one\n# two", "\n# one\n\n# two\n"] {
        assert!(
            parse_execution_plan(input, Arc::clone(&env))
                .unwrap()
                .lists
                .is_empty(),
            "{input}"
        );
    }
}

#[test]
fn alias_rewriting_never_enters_comments_and_reparses_introduced_comments() {
    let env = Environment::new();
    env.write()
        .variable_state
        .alias
        .insert("bad".into(), "if false; then echo WRONG; fi".into());
    env.write()
        .variable_state
        .alias
        .insert("ok".into(), "echo GOOD # ignored".into());
    let input = "echo OK # bad; echo $(bad)\n echo NEXT";
    assert_eq!(rewrite_aliases(input, Arc::clone(&env)).unwrap(), input);
    assert_eq!(
        parse_execution_plan(input, Arc::clone(&env))
            .unwrap()
            .lists
            .len(),
        2
    );
    let plan = parse_execution_plan("ok; if false", Arc::clone(&env))
        .expect_err("raw unsupported syntax is rejected before aliases");
    assert!(plan.to_string().contains("unsupported control keyword"));
    let plan = parse_execution_plan("ok; echo hidden", Arc::clone(&env)).unwrap();
    assert_eq!(plan.lists.len(), 1);
    assert_eq!(plan.lists[0].jobs.len(), 1);
    assert!(parse_execution_plan("echo $(ok\n)", env).is_ok());
}

#[test]
fn newline_after_comments_retains_whole_input_validation() {
    let env = Environment::new();
    for input in [
        "echo before # ignored\nif false",
        "echo before # ignored\necho )",
        "echo before; > # missing",
    ] {
        let err = parse_execution_plan(input, Arc::clone(&env)).expect_err(input);
        assert!(err.to_string().contains("syntax error"), "{err}");
    }
    let plan = parse_execution_plan(
        "echo one & # comment\n# between\necho two\n\necho three # tail",
        env,
    )
    .unwrap();
    assert_eq!(plan.lists.len(), 3);
    assert_eq!(
        plan.lists[0].execution,
        crate::shell::plan::ListExecutionMode::Asynchronous
    );
    assert_eq!(
        plan.lists[1].execution,
        crate::shell::plan::ListExecutionMode::Foreground
    );
}

#[test]
fn planned_sources_exclude_ignored_quotes_and_operators() {
    for input in [
        "echo a | # ignored \" ; rm -rf forbidden\ncat",
        "echo $(echo a # ignored \" ; rm -rf forbidden\n)",
    ] {
        let plan = parse_execution_plan(input, Environment::new()).unwrap();
        assert!(!plan.lists[0].source.contains("forbidden"));
        assert!(!plan.lists[0].jobs[0].source.contains("forbidden"));
    }
}

#[test]
fn shell_comments_do_not_change_lisp_or_structured_pipe_data() {
    for input in [
        "echo '[]' |: (list \"#quoted\" #t)",
        "echo '[]' |: where name == \"#quoted\"",
        "echo '[]' |: where name == a#b",
    ] {
        let base = parse_execution_plan(input, Environment::new()).unwrap();
        let commented =
            parse_execution_plan(&format!("{input} # ignored; if \""), Environment::new()).unwrap();
        assert_eq!(
            base.lists[0].jobs[0].struct_pipe_exprs, commented.lists[0].jobs[0].struct_pipe_exprs,
            "{input}"
        );
        assert_eq!(commented.lists.len(), 1);
    }
}
