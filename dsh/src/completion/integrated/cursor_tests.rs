use super::*;
// Synthetic environment and embedded definitions: never execute user completers.
fn cursor_engine() -> IntegratedCompletionEngine {
    let environment = Environment::new_synthetic();
    environment.write().variable_state.paths.clear();
    let engine = IntegratedCompletionEngine::new(environment);
    for data in [
        include_str!("../../../../completions/git.json"),
        include_str!("../../../../completions/bg.json"),
        include_str!("../../../../completions/sudo.json"),
    ] {
        engine
            .command_completion
            .lock()
            .add_command(serde_json::from_str(data).unwrap());
    }
    engine.set_shell_jobs(vec![(1, "synthetic-job".into(), "Stopped".into())]);
    engine
}

#[tokio::test]
async fn cursor_dynamic_separator_regression() {
    let engine = cursor_engine();
    let cwd = tempfile::tempdir().unwrap();
    let single = "bg %";
    let baseline = engine
        .complete(single, single.chars().count(), cwd.path(), 20, None)
        .await;
    assert!(
        baseline.candidates.iter().any(|c| c.text == "%1"),
        "control: {baseline:?}"
    );
    let compound = "echo ok && bg %";
    let result = engine
        .complete(compound, compound.chars().count(), cwd.path(), 20, None)
        .await;
    assert!(
        result.candidates.iter().any(|c| c.text == "%1"),
        "separator loses shell.job provider: {result:?}"
    );
}

#[test]
fn cursor_unicode_regression() {
    let input = "git checkout あい later";
    let cursor = "git checkout あ".chars().count();
    let engine = cursor_engine();
    let parsed = engine.convert_to_parsed_command_line(input, cursor);
    assert_eq!(parsed.current_token, "あ");
}

#[test]
fn cursor_wrapper_middle_regression() {
    let engine = cursor_engine();
    let marked = "sudo git checkout ma§ster later";
    let pos = marked.find('§').unwrap();
    let input = marked.replace('§', "");
    let parsed = engine.convert_to_parsed_command_line(&input, pos);
    let target = engine.dynamic_provider_target(&parsed);
    assert_eq!(target.command, "git");
    assert_eq!(target.current_token, "ma");
}

#[tokio::test]
async fn cursor_movement_does_not_reuse_other_token_candidates() {
    let engine = cursor_engine();
    let definition = r#"{"command":"probe","arguments":[{"name":"value","multiple":true,"type":{"type":"Choice","data":["aaaa","bbbb"]}}]}"#;
    engine
        .command_completion
        .lock()
        .add_command(serde_json::from_str(definition).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let input = "probe aa bb";
    let first = engine
        .complete(input, "probe aa".len(), dir.path(), 50, None)
        .await;
    assert!(
        first.candidates.iter().any(|c| c.text == "aaaa"),
        "control: {first:?}"
    );
    let second = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    println!("first={first:?}, second={second:?}");
    assert!(
        second.candidates.iter().any(|c| c.text == "bbbb"),
        "cursor movement reused the prior token's candidates: {second:?}"
    );
}

fn at(engine: &IntegratedCompletionEngine, marked: &str) -> (String, usize, ParsedCommandLine) {
    let byte = marked.find('§').unwrap();
    let cursor = marked[..byte].chars().count();
    let input = marked.replace('§', "");
    let parsed = engine.convert_to_parsed_command_line(&input, cursor);
    (input, cursor, parsed)
}

#[tokio::test]
async fn separator_matrix_keeps_providers_ranges_and_neighbors_together() {
    let engine = cursor_engine();
    let dir = tempfile::tempdir().unwrap();
    for separator in ["&&", "||", "|", ";", "\n", "\r\n", "&"] {
        for padding in ["", " ", "\t"] {
            for (left, right) in [
                ("echo ok", ""),
                ("echo 日本語🙂", " && echo later --different"),
            ] {
                let marked = format!("{left}{padding}{separator}{padding}bg %§x{right}");
                let (input, cursor, parsed) = at(&engine, &marked);
                assert_eq!(parsed.command, "bg", "{marked:?}");
                assert_eq!(parsed.current_token, "%");
                assert_eq!(parsed.raw_args, ["%"]);
                let result = engine.complete(&input, cursor, dir.path(), 30, None).await;
                assert!(
                    result.candidates.iter().any(|c| c.text == "%1"),
                    "{marked}: {result:?}"
                );
                let range = result.replacement_range.unwrap();
                let replaced = replace_char_range(&input, range.start, range.end, "%1");
                assert_eq!(
                    replaced,
                    format!("{left}{padding}{separator}{padding}bg %1{right}")
                );
                assert!(top_cache_empty(&engine, &input, dir.path()));
            }
        }
        let (input, cursor, parsed) =
            at(&engine, &format!("bg %§{separator}echo later --different"));
        assert_eq!(parsed.command, "bg");
        assert!(
            !parsed
                .specified_options
                .contains(&"--different".to_string())
        );
        assert!(
            engine
                .complete(&input, cursor, dir.path(), 30, None)
                .await
                .candidates
                .iter()
                .any(|c| c.text == "%1")
        );
    }
}

fn top_cache_empty(engine: &IntegratedCompletionEngine, input: &str, cwd: &Path) -> bool {
    engine
        .cache
        .lookup_scoped(engine.current_cache_scope(cwd), input)
        .is_none()
}

#[test]
fn literal_and_opaque_syntax_never_selects_inner_commands() {
    let engine = cursor_engine();
    for prefix in [
        r#"echo 'a && b'"#,
        r#"echo "a || b""#,
        r"echo a\&\&b",
        r"echo 'a\'&&b'",
        "echo ${X:-a&&b}",
        "echo $((1|2))",
        "echo $(git status)",
        "echo <(git status)",
        "echo (git status)",
        "echo a\u{a0}b",
        "echo ok\\\n tail",
    ] {
        let (_, _, parsed) = at(&engine, &format!("{prefix} && git checkout ma§"));
        assert_eq!(parsed.command, "git", "{prefix}");
        assert!(
            matches!(engine.argument_type_for_completion_context(&parsed), Some(ArgumentType::Dynamic { provider, .. }) if provider == "git.checkout_target")
        );
    }
    for marked in [
        "echo 'a && b§",
        "echo \"a || b§",
        r"echo a\&\&b§",
        "echo $(git sta§tus)",
    ] {
        let (_, _, parsed) = at(&engine, marked);
        assert_eq!(parsed.command, "echo");
    }
}

#[tokio::test]
async fn suppressed_regions_do_not_reach_any_candidate_source() {
    let engine = cursor_engine();
    let dir = tempfile::tempdir().unwrap();
    for marked in [
        "bg % &§& bg %",
        "bg % |§| bg %",
        "echo # bg %§",
        "echo |: select bg %§",
        "echo |>§",
        "echo |& bg %§",
        "echo &&& bg %§",
        "echo ||| bg %§",
    ] {
        let (input, cursor, _) = at(&engine, marked);
        let result = engine.complete(&input, cursor, dir.path(), 20, None).await;
        assert!(result.candidates.is_empty(), "{marked}");
        assert!(result.replacement_range.is_none());
        assert!(
            engine
                .ghost_completion(&input, cursor, dir.path(), None)
                .is_none()
        );
    }
}

#[test]
fn command_gaps_and_command_prefixes_have_character_ranges() {
    let engine = cursor_engine();
    for marked in [
        "echo &&§",
        "echo &&  §",
        "echo &&§git status",
        "echo &&  §git status",
        "echo && gi§t status",
    ] {
        let (input, cursor, parsed) = at(&engine, marked);
        assert_eq!(
            parsed.completion_context,
            parser::CompletionContext::Command,
            "{marked}"
        );
        assert_eq!(
            parsed.current_token,
            if marked.contains("gi§") { "gi" } else { "" }
        );
        let range = completion_replacement_range(&input, cursor, &parsed).unwrap();
        assert_eq!(
            slice_chars(&input, range.start, range.end),
            if input.ends_with("git status") {
                "git"
            } else {
                ""
            }
        );
    }
}

#[test]
fn unicode_bytes_combining_marks_and_duplicate_tokens_keep_identity() {
    let engine = cursor_engine();
    for marked in [
        "echo 日🙂 && git checkout あ§い later",
        "git checkout e\u{301}§x later",
        "sudo git checkout ma§ster ma",
        "sudo -u git git checkout ma§ster ma",
        "sudo sudo git checkout ma§ster ma",
    ] {
        let (_, _, parsed) = at(&engine, marked);
        let target = engine.dynamic_provider_target(&parsed);
        let expected = if marked.contains('あ') {
            "あ"
        } else if marked.contains('\u{301}') {
            "e\u{301}"
        } else {
            "ma"
        };
        assert_eq!(target.current_token, expected, "{marked}");
        assert!(
            matches!(
                target.completion_context,
                parser::CompletionContext::Argument { arg_index: 0, .. }
            ),
            "{marked}: {target:?}"
        );
    }
    let input = "git checkout あい";
    for byte in [14, 15, 16, 17, 18, usize::MAX] {
        let parsed = CommandLineParser::new().parse(input, byte);
        assert!(
            input.is_char_boundary(crate::completion::command_segment::floor_byte(input, byte))
        );
        assert_eq!(parsed.command, "git");
    }
    for marked in ["git checkout ma§ ma", "git checkout ma ma§"] {
        let (_, _, parsed) = at(&engine, marked);
        let expected = usize::from(marked.ends_with('§'));
        assert!(
            matches!(parsed.completion_context, parser::CompletionContext::Argument { arg_index, .. } if arg_index == expected),
            "{marked}: {parsed:?}"
        );
    }
}

#[test]
fn ghost_after_separator_uses_safe_cached_only_candidates() {
    let engine = cursor_engine();
    let dir = tempfile::tempdir().unwrap();
    let input = "echo ok && git che";
    assert_eq!(
        engine
            .ghost_completion(input, input.chars().count(), dir.path(), None)
            .as_deref(),
        Some("echo ok && git checkout")
    );
    assert!(!engine.dynamic.has_pending_refresh());
}

#[test]
fn option_value_ranges_preserve_prefix_and_unicode_neighbors() {
    let engine = cursor_engine();
    let definition = r#"{"command":"probe","global_options":[{"long":"--value","short":"-v","takes_value":true,"value_type":{"type":"Choice","data":["alpha","beta"]}}]}"#;
    // Use the same serialized shape as real options, through the public loader types.
    let mut value: serde_json::Value = serde_json::from_str(definition).unwrap();
    value["global_options"][0]["takes_value"] = serde_json::json!(true);
    engine
        .command_completion
        .lock()
        .add_command(serde_json::from_value(value).unwrap());
    for (marked, expected) in [
        ("echo 日 && probe --value=a§lpha && echo end", "alpha"),
        ("echo 日 && probe -va§lpha && echo end", "alpha"),
    ] {
        let (input, cursor, parsed) = at(&engine, marked);
        assert!(
            matches!(
                parsed.completion_context,
                parser::CompletionContext::OptionValue { .. }
            ),
            "{parsed:?}"
        );
        let range = completion_replacement_range(&input, cursor, &parsed).unwrap();
        assert_eq!(slice_chars(&input, range.start, range.end), expected);
    }
}

#[test]
fn script_templates_receive_only_active_command_arguments() {
    use crate::completion::generators::script::{ScriptGenerator, ScriptRunner};
    struct Capture;
    impl ScriptRunner for Capture {
        fn run(&self, command: &str) -> anyhow::Result<String> {
            assert_eq!(command, "'probe' 'token' 'token'");
            Ok("token-result\n".into())
        }
    }
    let engine = cursor_engine();
    engine.command_completion.lock().add_command(
        serde_json::from_str(r#"{"command":"probe","arguments":[{"name":"value"}]}"#).unwrap(),
    );
    let (_, _, parsed) = at(&engine, "echo before && probe token§ && echo after");
    let candidates = ScriptGenerator::new(Capture)
        .generate_script_candidates("$COMMAND $CURRENT_TOKEN $ARG_0", &parsed)
        .unwrap();
    assert_eq!(candidates[0].text, "token-result");
}

#[tokio::test]
async fn structured_continuations_and_special_trailing_gaps_stay_suppressed() {
    use crate::parser::{Rule, ShellParser};
    use pest::Parser;
    let engine = cursor_engine();
    let dir = tempfile::tempdir().unwrap();
    let mut escaped_regions = Vec::new();
    for marked in [
        "echo |:\n bg %§",
        "echo |:\r\n bg %§",
        "echo |: # comment\n bg %§",
        "bg % |> §",
        "bg % |: (foo) §",
        "bg % |: (foo) § && bg %",
    ] {
        let (input, cursor, _) = at(&engine, marked);
        let pair = ShellParser::parse(Rule::commands, &input)
            .unwrap()
            .next()
            .unwrap();
        assert_eq!(
            pair.as_span().end(),
            input.len(),
            "must test supported execution syntax: {marked}"
        );
        let result = engine.complete(&input, cursor, dir.path(), 20, None).await;
        if result.replacement_range.is_some()
            || !result.candidates.is_empty()
            || engine
                .ghost_completion(&input, cursor, dir.path(), None)
                .is_some()
        {
            escaped_regions.push(marked);
        }
    }
    assert!(
        escaped_regions.is_empty(),
        "regions escaped suppression: {escaped_regions:?}"
    );
    // A real separator ends the special region and permits the next invocation.
    for marked in [
        "echo |> ; bg %§",
        "echo |: (foo) && bg %§",
        "echo |:\n select pid; bg %§",
    ] {
        let (input, cursor, _) = at(&engine, marked);
        assert!(
            engine
                .complete(&input, cursor, dir.path(), 20, None)
                .await
                .candidates
                .iter()
                .any(|c| c.text == "%1"),
            "{marked}"
        );
    }
}

#[tokio::test]
async fn variable_syntax_validates_names_and_preserves_quote_context() {
    use skim::SkimItem;
    let environment = Environment::new_synthetic();
    {
        let mut env = environment.write();
        env.variable_state.paths.clear();
        for name in ["SAFE_NAME", "SAFE_BAD;echo", "1POSITIONAL", "SAFE_界"] {
            env.variable_state
                .variables
                .insert(name.into(), "value".into());
        }
    }
    let engine = IntegratedCompletionEngine::new(environment);
    for (raw, expected) in [("$SAFE_N", "$SAFE_NAME"), ("\"$SAFE_N\"", "\"$SAFE_NAME\"")] {
        let input = format!("echo {raw}");
        let result = engine
            .complete(
                &input,
                input.chars().count(),
                std::path::Path::new("."),
                50,
                None,
            )
            .await;
        let candidates = engine.to_candidates(result.candidates);
        let formatted =
            crate::completion::shell_path::format_candidates_for_token(candidates, Some(raw));
        assert!(
            formatted
                .iter()
                .any(|candidate| candidate.output() == expected),
            "{raw:?}: {formatted:?}"
        );
    }
    // Single quotes suppress variable syntax; literal filenames are covered
    // by quoted_literal_names_are_not_variable_or_home_references.
    let input = "echo '$SAFE_N'";
    let result = engine
        .complete(input, input.len(), std::path::Path::new("."), 50, None)
        .await;
    assert!(
        result
            .candidates
            .iter()
            .all(|candidate| candidate.candidate_type != CandidateType::VariableReference)
    );
    let names = engine.variable_candidates("", |name| format!("${name}"));
    assert!(names.iter().any(|candidate| candidate.text == "$SAFE_NAME"));
    assert!(
        names
            .iter()
            .all(|candidate| !["$SAFE_BAD;echo", "$1POSITIONAL", "$SAFE_界"]
                .contains(&candidate.text.as_str())),
        "{names:?}"
    );
}

#[test]
fn real_home_file_candidates_preserve_expansion_and_literal_tail() {
    let _guard = crate::test_env_lock();
    use crate::completion::generators::filesystem::FileSystemGenerator;
    use crate::completion::shell_path::format_candidates_for_token;
    use skim::SkimItem;
    let home = std::env::var("HOME").unwrap();
    let fixture = tempfile::tempdir_in(&home).unwrap();
    std::fs::write(fixture.path().join("probe a;*.txt"), "fixture").unwrap();
    let prefix = format!(
        "~/{}/probe",
        fixture.path().file_name().unwrap().to_str().unwrap()
    );
    let candidates = FileSystemGenerator::generate_file_candidates(&prefix).unwrap();
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.text.ends_with("probe a;*.txt"))
        .unwrap();
    // Exercise the actual CompletionCandidate -> engine conversion.
    let engine = IntegratedCompletionEngine::new(crate::environment::Environment::new_synthetic());
    let enhanced = engine.convert_to_enhanced_candidate(candidate.clone());
    let formatted = format_candidates_for_token(vec![enhanced.to_candidate()], Some(&prefix));
    assert!(
        formatted[0].output().starts_with("~/"),
        "{:?}",
        formatted[0]
    );
    assert!(formatted[0].output().ends_with(r"probe\ a\;\*.txt"));
    assert_eq!(formatted[0].get_display_name(), candidate.text);
    assert_eq!(enhanced.candidate_type, CandidateType::HomeFile);
    assert_eq!(
        enhanced.candidate_type.sort_order(),
        CandidateType::File.sort_order()
    );
    assert_eq!(enhanced.candidate_type.icon(), CandidateType::File.icon());
    for literal in [
        format!("'{prefix}'"),
        format!("\"{prefix}\""),
        format!("\\{prefix}"),
    ] {
        let values = FileSystemGenerator::generate_file_candidates(&literal).unwrap();
        assert!(
            values.iter().all(|value| value.text != candidate.text),
            "{literal}: {values:?}"
        );
        assert!(
            // A missing literal directory can be uncached I/O failure or a
            // cached empty listing; neither may offer the HOME-backed value.
            crate::completion::path::path_completion_prefix_for_shell_token(&literal, false, None)
                .ok()
                .flatten()
                .is_none(),
            "literal ghost lookup: {literal}"
        );
    }
    let ghost = crate::completion::shell_path::format_path_for_token(&candidate.text, &prefix);
    assert_eq!(ghost, formatted[0].output());
}
