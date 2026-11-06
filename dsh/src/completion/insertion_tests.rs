//! Completion values pass through real generators, display conversion, editor
//! insertion and strict execution parsing. No resulting command is executed.
use super::command::{CommandCompletionDatabase, CompletionCandidate};
use super::display::{Candidate, CompletionConfig, CompletionDisplay};
use super::framework::{CompletionFrameworkKind, CompletionSelection, SkimCompletionFramework};
use super::generator::CompletionGenerator;
use super::integrated::{CandidateType, EnhancedCandidate};
use super::parser::CommandLineParser;
use super::shell_path::format_candidates_for_token;
use crate::input::{Input, InputConfig};
use crate::parser::{Rule, ShellParser};
use pest::{Parser, iterators::Pair};
use skim::SkimItem;

fn inspect(pair: Pair<'_, Rule>, values: &mut Vec<String>) {
    assert!(!matches!(
        pair.as_rule(),
        Rule::glob_word
            | Rule::brace_word
            | Rule::variable
            | Rule::command_subst
            | Rule::proc_subst
            | Rule::comment
            | Rule::subshell
    ));
    if pair.as_rule() == Rule::span {
        values.push(crate::parser::get_string(pair).unwrap());
    } else {
        for inner in pair.into_inner() {
            inspect(inner, values);
        }
    }
}

fn assert_word(output: &str, expected: &str) {
    let original = "echo 界 OLD suffix";
    let mut input = Input::new(InputConfig::default());
    input.reset(original.to_string());
    input.replace_range_chars(7, 10, output);
    let source = input.as_str();
    let pair = ShellParser::parse(Rule::commands, source)
        .unwrap()
        .next()
        .unwrap();
    assert_eq!(pair.as_span().end(), source.len(), "{source:?}");
    // Check expansion nodes separately from extracting the outer argv spans.
    fn no_expansion(pair: Pair<'_, Rule>) {
        assert!(
            !matches!(
                pair.as_rule(),
                Rule::glob_word
                    | Rule::brace_word
                    | Rule::variable
                    | Rule::command_subst
                    | Rule::proc_subst
                    | Rule::comment
                    | Rule::subshell
            ),
            "{:?}: {:?}",
            pair.as_rule(),
            pair.as_str()
        );
        for child in pair.into_inner() {
            no_expansion(child);
        }
    }
    no_expansion(pair.clone());
    let mut values = Vec::new();
    inspect(pair, &mut values);
    assert_eq!(values, ["echo", "界", expected, "suffix"], "{source:?}");
}

#[test]
fn real_choice_conversion_selection_and_insertion_round_trip() {
    let values = [
        "X=value",
        "hello world",
        "a;b",
        "*.rs",
        "?x",
        "[ab]",
        "x{a,b}",
        "#tag",
        "~root",
        "",
        "a'b",
        "a\\b",
        "tail\\",
        "\\'",
        "a\"$x",
        "界 e\u{301}",
        "two\nlines",
    ];
    let mut database = CommandCompletionDatabase::new();
    database.add_command(serde_json::from_value(serde_json::json!({"command":"probe", "arguments":[{"name":"value", "type":{"type":"Choice", "data":values}}]})).unwrap());
    let parsed = CommandLineParser::new().parse("probe ", 6);
    let generated = CompletionGenerator::new(&database)
        .generate_candidates(&parsed)
        .unwrap();
    assert_eq!(generated.len(), values.len());
    for value in generated {
        assert!(value.description.is_none());
        for description in [None, Some("display only".to_string())] {
            let enhanced = EnhancedCandidate {
                text: value.text.clone(),
                description,
                candidate_type: CandidateType::Argument,
                priority: value.priority,
            };
            for raw in ["", "'old'", "\"old\"", "old\\ word"] {
                let candidates =
                    format_candidates_for_token(vec![enhanced.to_candidate()], Some(raw));
                assert_eq!(candidates[0].get_display_name(), value.text);
                assert_word(&candidates[0].output(), &value.text);
                for framework in [
                    CompletionFrameworkKind::Inline,
                    CompletionFrameworkKind::Skim,
                ] {
                    let selected = super::select_completion_items_with_framework(
                        candidates.clone(),
                        None,
                        "",
                        "",
                        CompletionConfig::default(),
                        framework,
                    );
                    assert_eq!(
                        selected,
                        CompletionSelection::Selected(candidates[0].output().to_string())
                    );
                }
                let display =
                    CompletionDisplay::new(candidates.clone(), String::new(), String::new());
                assert_word(&display.get_selected().unwrap().output(), &value.text);
                let skim = SkimCompletionFramework::run_with_skim(candidates, None).unwrap();
                assert_word(&skim, &value.text);
            }
        }
    }
}

#[test]
fn explicit_syntax_and_literal_values_remain_distinct_in_quotes() {
    for (raw, syntax_expected) in [
        ("", "$HOME"),
        ("\"old\"", "\"$HOME\""),
        ("'old'", "'$HOME'"),
    ] {
        let syntax = Candidate::ShellSyntax {
            kind: super::display::ShellSyntaxKind::Variable,
            text: "$HOME".into(),
            description: None,
        };
        let formatted = format_candidates_for_token(vec![syntax], Some(raw));
        assert_eq!(formatted[0].output(), syntax_expected);
        let literal =
            format_candidates_for_token(vec![Candidate::Basic("$HOME".into())], Some(raw));
        assert_word(&literal[0].output(), "$HOME");
    }
    for raw in ["", "\"old\"", "'old'"] {
        let syntax = Candidate::ShellSyntax {
            kind: super::display::ShellSyntaxKind::Home,
            text: "~root".into(),
            description: None,
        };
        let output = format_candidates_for_token(vec![syntax], Some(raw))[0]
            .output()
            .to_string();
        if raw.is_empty() {
            assert_eq!(output, "~root");
        } else {
            assert_word(&output, "~root");
        }
    }
    let file = Candidate::File {
        path: "dir with space/".into(),
        is_dir: true,
    };
    let prepared = format_candidates_for_token(vec![file.clone()], None);
    assert_eq!(prepared[0].get_display_name(), file.get_display_name());
    assert!(prepared[0].output().ends_with('/'));
    assert_eq!(prepared[0].get_type_char(), file.get_type_char());
    assert_eq!(
        format_candidates_for_token(prepared.clone(), Some("'other'")),
        prepared
    );
}

#[test]
fn real_script_output_description_does_not_change_insertion() {
    use super::generators::script::{ScriptGenerator, ScriptRunner};
    struct Output;
    impl ScriptRunner for Output {
        fn run(&self, _: &str) -> anyhow::Result<String> {
            Ok("hello world\na;b\tDescription\n*.rs\n#tag\tdescription".into())
        }
    }
    let parsed = CommandLineParser::new().parse("probe ", 6);
    let values: Vec<CompletionCandidate> = ScriptGenerator::new(Output)
        .generate_script_candidates("mock only", &parsed)
        .unwrap();
    for value in values {
        let candidate = EnhancedCandidate {
            text: value.text.clone(),
            description: value.description,
            candidate_type: CandidateType::Argument,
            priority: 0,
        };
        assert_word(
            &format_candidates_for_token(vec![candidate.to_candidate()], None)[0].output(),
            &value.text,
        );
    }
}
