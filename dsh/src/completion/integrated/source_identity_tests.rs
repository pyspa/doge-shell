use super::*;
use crate::completion::parser::CompletionContext;
use pest::Parser;

fn engine() -> IntegratedCompletionEngine {
    let environment = Environment::new_synthetic();
    environment.write().variable_state.paths.clear();
    let engine = IntegratedCompletionEngine::new(environment);
    for data in [
        include_str!("../../../../completions/git.json"),
        include_str!("../../../../completions/sudo.json"),
        include_str!("../../../../completions/env.json"),
    ] {
        engine
            .command_completion
            .lock()
            .add_command(serde_json::from_str(data).unwrap());
    }
    engine
}

fn at(engine: &IntegratedCompletionEngine, marked: &str) -> (String, usize, ParsedCommandLine) {
    let cursor = marked[..marked.find('§').unwrap()].chars().count();
    let input = marked.replace('§', "");
    let parsed = engine.convert_to_parsed_command_line(&input, cursor);
    (input, cursor, parsed)
}

#[test]
fn literal_command_parents_and_options_keep_their_identity() {
    let engine = engine();
    for marked in [
        "git remote r§ename",
        "'git' remote r§ename",
        r"g\it remote r§ename",
        "g\"it\" remote r§ename",
        "git 'remote' r§ename",
        "git \"remote\" r§ename",
        r"git re\mote r§ename",
        "git '-C' /tmp remote r§ename",
        "git \"-C\" /tmp remote r§ename",
        "git -C /tmp 'remote' r§ename",
        "echo 界 && 'git' remote r§ename later",
    ] {
        let (_, _, parsed) = at(&engine, marked);
        assert_eq!(parsed.command, "git", "{marked}: {parsed:?}");
        assert_eq!(parsed.subcommand_path, ["remote"], "{marked}: {parsed:?}");
        let db = engine.command_completion.lock();
        let candidates = CompletionGenerator::new(&db)
            .generate_candidates(&parsed)
            .unwrap();
        assert!(
            candidates.iter().any(|c| c.text == "rename"),
            "{marked}: {candidates:?}"
        );
        assert!(!candidates.iter().any(|c| c.text == "rebase"), "{marked}");
    }
}

#[test]
fn prefix_commands_keep_absolute_source_replacement_and_wrapper_context() {
    let engine = engine();
    for prefix in [
        "X=value ",
        "X='界 space' Y=two ",
        ">out ",
        "2>&1 ",
        "X=value >out Y=two ",
    ] {
        for leader in ["", "echo 界 && "] {
            for wrapper in ["", "sudo -n ", "env KEY=value "] {
                let marked = format!("{leader}{prefix}{wrapper}git remote r§ename later");
                let (input, cursor, parsed) = at(&engine, &marked);
                let target = engine.dynamic_provider_target(&parsed);
                assert_eq!(target.command, "git", "{marked}: {target:?}");
                assert_eq!(target.subcommand_path, ["remote"], "{marked}: {target:?}");
                let range = token_range_at_cursor(&input, cursor).unwrap();
                assert_eq!(
                    slice_chars(&input, range.start, range.end),
                    "rename",
                    "{marked}"
                );
                assert_eq!(
                    replace_char_range(&input, range.start, range.end, "remove"),
                    marked.replace("r§ename", "remove")
                );
            }
        }
    }
}

#[test]
fn assignment_source_syntax_is_not_inferred_from_decoded_equals() {
    let engine = engine();
    for marked in [
        "'X=value' arg§",
        r"X\=value arg§",
        "X'='value arg§",
        "\"X=value\" arg§",
    ] {
        let (_, _, parsed) = at(&engine, marked);
        assert_eq!(parsed.command, "X=value", "{marked}: {parsed:?}");
    }
    // The actual grammar allows quoting on the RHS, after the literal '='.
    let (_, _, parsed) = at(&engine, "X='value' git remote r§ename");
    assert_eq!(parsed.command, "git", "{parsed:?}");
}

#[test]
fn prefix_cursor_ownership_does_not_turn_assignments_or_descriptors_into_commands() {
    let engine = engine();
    for marked in [
        "X=va§lue git remote rename",
        "X=§ git remote rename",
        "2>&§1 git remote rename",
        "2>§&1 git remote rename",
    ] {
        let (input, cursor, parsed) = at(&engine, marked);
        assert!(
            parsed.source.as_ref().unwrap().suppressed,
            "{marked}: {parsed:?}"
        );
        assert!(token_range_at_cursor(&input, cursor).is_none(), "{marked}");
    }
    for marked in [
        "X=value §",
        ">out §",
        "X=value >out §",
        "X=value §git remote rename",
    ] {
        let (_, _, parsed) = at(&engine, marked);
        assert_eq!(
            parsed.completion_context,
            CompletionContext::Command,
            "{marked}: {parsed:?}"
        );
        assert_eq!(parsed.current_token, "", "{marked}");
    }
    for marked in [">§out git remote rename", "X=value > §", "X=value >§", ">§"] {
        let (input, cursor, parsed) = at(&engine, marked);
        assert!(
            !parsed.source.as_ref().unwrap().suppressed,
            "{marked}: {parsed:?}"
        );
        assert!(
            matches!(
                parsed.completion_context,
                CompletionContext::Argument {
                    arg_type: Some(ArgumentType::File { .. }),
                    ..
                }
            ),
            "{marked}: {parsed:?}"
        );
        assert!(token_range_at_cursor(&input, cursor).is_some(), "{marked}");
    }
}

#[tokio::test]
async fn literal_option_names_preserve_value_query_and_insertion_source() {
    use skim::SkimItem;
    let engine = engine();
    engine.command_completion.lock().add_command(serde_json::from_value(serde_json::json!({
        "command":"pick", "global_options":[{"long":"--mode","takes_value":true,"value_type":{"type":"Choice","data":["alpha","a b'c"]}}]
    })).unwrap());
    let dir = tempfile::tempdir().unwrap();
    for marked in [
        "pick '--mode' al§pha tail",
        "pick '--mode'=al§pha tail",
        "pick --mode='al§pha' tail",
        "pick \"--mode=al§pha\" tail",
    ] {
        let (input, cursor, parsed) = at(&engine, marked);
        assert!(
            matches!(parsed.completion_context, CompletionContext::OptionValue { ref option_name, .. } if option_name == "--mode"),
            "{marked}: {parsed:?}"
        );
        let result = engine.complete(&input, cursor, dir.path(), 20, None).await;
        assert!(
            result.candidates.iter().any(|c| c.text == "alpha"),
            "{marked}: {result:?}"
        );
        let range = result.replacement_range.unwrap();
        // Exercise the exact presentation/insertion edge used by the key handler.
        let candidates = crate::completion::shell_path::format_candidates_for_range(
            vec![crate::completion::display::Candidate::Basic("a b'c".into())],
            &input,
            range,
        );
        let replaced = replace_char_range(&input, range.start, range.end, &candidates[0].output());
        let pairs =
            crate::parser::ShellParser::parse(crate::parser::Rule::commands, &replaced).unwrap();
        assert_eq!(
            pairs.clone().next().unwrap().as_span().end(),
            replaced.len(),
            "{marked}: {replaced:?}"
        );
        let words: Vec<_> = pairs
            .flatten()
            .filter(|p| p.as_rule() == crate::parser::Rule::span)
            .filter_map(crate::parser::get_string)
            .collect();
        assert!(
            words.contains(&"--mode=a b'c".into()) || words.contains(&"a b'c".into()),
            "{marked}: {replaced:?}: {words:?}"
        );
    }
}

#[test]
fn malformed_prefix_redirects_do_not_dispatch_a_later_command() {
    let engine = engine();
    for marked in [
        "> >out git che§ckout",
        "X=value>out git che§ckout",
        ">out>other git che§ckout",
        "2>&'1' git che§ckout",
    ] {
        let input = marked.replace('§', "");
        let complete = crate::parser::ShellParser::parse(crate::parser::Rule::commands, &input)
            .ok()
            .and_then(|mut pairs| pairs.next())
            .is_some_and(|pair| pair.as_span().end() == input.len());
        assert!(!complete, "invalid prefix accepted by execution: {marked}");
        let (_, _, parsed) = at(&engine, marked);
        assert!(
            parsed.source.as_ref().unwrap().suppressed,
            "{marked}: {parsed:?}"
        );
    }
}

#[test]
fn inline_single_quoted_option_value_can_end_in_a_literal_backslash() {
    use skim::SkimItem;
    let input = "'--mode=old'";
    let range = CompletionReplacementRange {
        start: 8,
        end: input.chars().count(),
    };
    let prepared = crate::completion::shell_path::format_candidates_for_range(
        vec![crate::completion::display::Candidate::Basic(
            "tail\\".into(),
        )],
        input,
        range,
    );
    let replaced = replace_char_range(input, range.start, range.end, &prepared[0].output());
    let word = crate::parser::ShellParser::parse(crate::parser::Rule::span, &replaced)
        .unwrap()
        .next()
        .unwrap();
    assert_eq!(word.as_span().end(), replaced.len(), "{replaced:?}");
    assert_eq!(
        crate::parser::get_string(word).as_deref(),
        Some("--mode=tail\\")
    );
}

#[test]
fn prefix_external_provider_receives_original_input_cursor_and_token() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = crate::completion::subprocess::external_process_test_guard();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("completer");
    std::fs::write(&script, "#!/bin/sh\nprintf '%s' \"$DOGESH_COMPLETION_INPUT\" > received-input\nprintf '%s' \"$DOGESH_COMPLETION_CURSOR\" > received-cursor\nprintf '%s' \"$DOGESH_COMPLETION_CURRENT_TOKEN\" > received-token\nprintf 'zzcandidate\\tfixture\\n'\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let engine = engine();
    engine.environment.write().set_shell_var(
        "DOGESH_EXTERNAL_COMPLETER".into(),
        script.display().to_string(),
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    engine.set_notifier(tx);
    let (input, cursor, _) = at(&engine, "X='界 space' >out 'unknown' zz§suffix tail");
    loop {
        let result = engine.complete(&input, cursor, dir.path(), 20, None).await;
        if result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "zzcandidate")
        {
            break;
        }
        tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("received-input")).unwrap(),
        input
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("received-cursor")).unwrap(),
        cursor.to_string()
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("received-token")).unwrap(),
        "zz"
    );
    });
}

#[test]
fn prefix_recognition_matches_execution_syntax_without_running_values() {
    let engine = engine();
    for prefix in [
        "X= ",
        "X='界 space' Y=two ",
        ">out ",
        "2>&1 ",
        "X=value >out Y=two ",
    ] {
        let input = format!("{prefix}git remote rename");
        let pairs =
            crate::parser::ShellParser::parse(crate::parser::Rule::commands, &input).unwrap();
        assert_eq!(
            pairs.clone().next().unwrap().as_span().end(),
            input.len(),
            "{input}"
        );
        let command = pairs
            .flatten()
            .find(|p| p.as_rule() == crate::parser::Rule::argv0)
            .unwrap();
        assert_eq!(command.as_str(), "git", "{input}");
        let (_, _, parsed) = at(&engine, &format!("{prefix}git remote r§ename"));
        assert_eq!(parsed.command, command.as_str());
    }
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("must-not-exist");
    let marked = format!("X=$(touch {}) git remote r§ename", marker.display());
    let (_, _, parsed) = at(&engine, &marked);
    assert_eq!(parsed.command, "git");
    assert!(!marker.exists());
}

#[test]
fn literal_backslashes_in_logical_names_are_never_decoded_twice() {
    let engine = engine();
    engine.command_completion.lock().add_command(serde_json::from_value(serde_json::json!({
        "command":"cmd\\name", "subcommands":[{"name":"sub\\name", "subcommands":[{"name":"rename"}]}]
    })).unwrap());
    let (_, _, parsed) = at(&engine, "'cmd\\name' 'sub\\name' r§ename");
    assert_eq!(parsed.command, "cmd\\name");
    assert_eq!(parsed.subcommand_path, ["sub\\name"]);
    let db = engine.command_completion.lock();
    let corrected = CompletionGenerator::new(&db).correct_parsed_command_line(&parsed);
    assert_eq!(corrected.command, parsed.command);
    assert_eq!(corrected.subcommand_path, parsed.subcommand_path);
    assert!(
        CompletionGenerator::new(&db)
            .generate_candidates(&corrected)
            .unwrap()
            .iter()
            .any(|candidate| candidate.text == "rename")
    );
}

#[test]
fn ordinary_fd_prefixes_follow_doge_execution_argv0() {
    let engine = engine();
    for marked in ["3>out git che§", "1>out git che§", "0<input git che§"] {
        let (input, _, parsed) = at(&engine, marked);
        let pairs =
            crate::parser::ShellParser::parse(crate::parser::Rule::commands, &input).unwrap();
        assert_eq!(pairs.clone().next().unwrap().as_span().end(), input.len());
        let command = pairs
            .flatten()
            .find(|p| p.as_rule() == crate::parser::Rule::argv0)
            .unwrap();
        assert_eq!(parsed.command, command.as_str(), "{marked}: {parsed:?}");
    }
    let (_, _, parsed) = at(&engine, "2>& 1 git che§");
    assert!(parsed.source.unwrap().suppressed);
}

#[test]
fn inherited_quote_context_is_used_for_option_value_queries() {
    let engine = engine();
    for marked in [
        "git --config-env=\"a'b§\"",
        "git \"--config-env=a'b§\"",
        "git \"--config-env=a'b\"§",
    ] {
        let (_, _, parsed) = at(&engine, marked);
        assert_eq!(parsed.completion_query(), "a'b", "{marked}: {parsed:?}");
        assert!(
            parsed.current_token.contains("a'b"),
            "raw token changed: {parsed:?}"
        );
    }
}

#[tokio::test]
async fn inherited_inline_quote_matches_choices_and_selector_query() {
    let engine = engine();
    engine.command_completion.lock().add_command(serde_json::from_value(serde_json::json!({
        "command":"pick", "global_options":[{"long":"--mode","takes_value":true,"value_type":{"type":"Choice","data":["a'b","other"]}}]
    })).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let (input, cursor, parsed) = at(&engine, "pick \"--mode=a'b\"§");
    let result = engine.complete(&input, cursor, dir.path(), 20, None).await;
    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "a'b"),
        "{result:?}"
    );
    let range = result.replacement_range.unwrap();
    assert_eq!(
        crate::completion::shell_path::query_for_range(&input, range.start, cursor),
        "a'b"
    );
    assert_eq!(parsed.current_token, "a'b\"");
}
