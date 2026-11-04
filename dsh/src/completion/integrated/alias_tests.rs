//! Public and parsed-context regressions for fixed-argument aliases.
use super::*;
use crate::completion::parser::CompletionContext;

fn engine() -> IntegratedCompletionEngine {
    let environment = Environment::new_synthetic();
    environment.write().variable_state.paths.clear();
    let engine = IntegratedCompletionEngine::new(environment);
    for json in [
        include_str!("../../../../completions/git.json"),
        include_str!("../../../../completions/sudo.json"),
        include_str!("../../../../completions/env.json"),
        r#"{"command":"pick","global_options":[{"long":"--mode","takes_value":true,"value_type":{"type":"Choice","data":["alpha","beta"]}}],"arguments":[{"name":"first","type":{"type":"Choice","data":["alpha-old"]}},{"name":"second","type":{"type":"Choice","data":["hello","hello world"]}}]}"#,
        r#"{"command":"other","arguments":[{"name":"first","type":{"type":"Choice","data":["alpha-new"]}}]}"#,
    ] {
        engine
            .command_completion
            .lock()
            .add_command(serde_json::from_str(json).unwrap());
    }
    engine
}

fn alias(engine: &IntegratedCompletionEngine, name: &str, replacement: &str) {
    engine
        .environment
        .write()
        .variable_state
        .alias
        .insert(name.into(), replacement.into());
}

fn at(engine: &IntegratedCompletionEngine, marked: &str) -> (String, usize, ParsedCommandLine) {
    let byte = marked.find('§').unwrap();
    let cursor = marked[..byte].chars().count();
    let input = marked.replace('§', "");
    let parsed = engine.convert_to_parsed_command_line(&input, cursor);
    (input, cursor, parsed)
}

#[tokio::test]
async fn alias_fixed_subcommand_keeps_public_options_and_original_range() {
    let engine = engine();
    alias(&engine, "auditg", "git commit");
    let dir = tempfile::tempdir().unwrap();
    for marked in [
        "auditg --ame§nd later",
        "echo 日本語 && auditg --ame§nd later",
        "sudo auditg --ame§nd later",
        "env TERM=x sudo auditg --ame§nd later",
        "X='界 space' >out auditg --ame§nd later",
        ">out auditg --ame§nd later",
        "X=value sudo auditg --ame§nd later",
    ] {
        let (input, cursor, parsed) = at(&engine, marked);
        let target = engine.dynamic_provider_target(&parsed);
        assert_eq!(target.command, "git", "{marked}: {target:?}");
        assert_eq!(target.subcommand_path, ["commit"]);
        assert_eq!(target.current_token, "--ame");
        let result = engine.complete(&input, cursor, dir.path(), 50, None).await;
        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "--amend"),
            "{marked}: {result:?}"
        );
        let range = result.replacement_range.unwrap();
        assert_eq!(slice_chars(&input, range.start, range.end), "--amend");
        assert_eq!(
            replace_char_range(&input, range.start, range.end, "--amend"),
            input
        );
    }
    assert_eq!(
        engine.ghost_completion("auditg --ame", 12, dir.path(), None),
        Some("auditg --amend".into())
    );
    assert_eq!(
        engine.ghost_completion("sudo auditg --ame", 17, dir.path(), None),
        Some("sudo auditg --amend".into())
    );
    let input = "X=value >out auditg --ame";
    assert_eq!(
        engine.ghost_completion(input, input.chars().count(), dir.path(), None),
        Some("X=value >out auditg --amend".into())
    );
}

#[tokio::test]
async fn alias_quoted_fixed_argument_preserves_cursor_role_and_suffix() {
    let engine = engine();
    alias(&engine, "apick", "pick 'fixed arg'");
    let dir = tempfile::tempdir().unwrap();
    for marked in [
        "apick \"he§llo\" tail",
        "apick 'he§llo' tail",
        "apick \"he§",
        "echo 日本語 && apick \"he§llo\" tail",
    ] {
        let (input, cursor, parsed) = at(&engine, marked);
        assert_eq!(parsed.command, "pick");
        assert!(
            matches!(
                parsed.completion_context,
                CompletionContext::Argument { arg_index: 1, .. }
            ),
            "{marked}: {parsed:?}"
        );
        assert!(parsed.raw_args.iter().any(|arg| arg == "'fixed arg'"));
        assert_eq!(parsed.completion_query(), "he");
        if marked.contains("tail") {
            assert!(parsed.raw_args.iter().any(|arg| arg == "tail"));
        }
        if marked.starts_with("echo") {
            assert!(parsed.source.as_ref().unwrap().multiple);
        }
        let result = engine.complete(&input, cursor, dir.path(), 50, None).await;
        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "hello"),
            "{marked}: {result:?}"
        );
        let range = result.replacement_range.unwrap();
        let expected_start = input.find("he").unwrap() - 1;
        assert_eq!(range.start, input[..expected_start].chars().count());
    }
}

#[test]
fn alias_respects_syntax_authority_and_command_cursor() {
    let engine = engine();
    alias(&engine, "auditg", "git commit");
    alias(&engine, "chain", "auditg");
    for (marked, expected) in [
        ("'auditg' --ame§", "auditg"),
        ("\"auditg\" --ame§", "auditg"),
        (r"audit\g --ame§", "auditg"),
        ("audit§g --amend", "audit"),
        ("auditg§ --amend", "auditg"),
        ("X=value audit§g --amend", "audit"),
        ("X=value >out auditg§ --amend", "auditg"),
        ("X=value 'auditg' --ame§", "auditg"),
        ("X=value >out \"auditg\" --ame§", "auditg"),
        ("chain --ame§", "auditg"),
    ] {
        let (_, _, parsed) = at(&engine, marked);
        assert_eq!(parsed.command, expected, "{marked}: {parsed:?}");
    }
}

#[tokio::test]
async fn alias_changes_invalidate_parse_result_and_ghost_immediately() {
    let engine = engine();
    let dir = tempfile::tempdir().unwrap();
    let input = "a al";
    alias(&engine, "a", "pick");
    let old_scope = engine.current_cache_scope(dir.path());
    let old = engine
        .complete(input, input.len(), dir.path(), 30, None)
        .await;
    assert!(
        old.candidates
            .iter()
            .any(|candidate| candidate.text == "alpha-old")
    );
    assert!(engine.cache.lookup_scoped(old_scope, input).is_some());
    assert_eq!(
        engine.ghost_completion(input, input.len(), dir.path(), None),
        Some("a alpha-old".into())
    );

    // Same input, same cursor, before any TTL expiry.
    alias(&engine, "a", "other");
    let parsed = engine.convert_to_parsed_command_line(input, input.len());
    assert_eq!(parsed.command, "other");
    assert_ne!(engine.current_cache_scope(dir.path()), old_scope);
    let new = engine
        .complete(input, input.len(), dir.path(), 30, None)
        .await;
    assert!(
        new.candidates
            .iter()
            .any(|candidate| candidate.text == "alpha-new")
    );
    assert!(
        !new.candidates
            .iter()
            .any(|candidate| candidate.text == "alpha-old")
    );
    assert_eq!(
        engine.ghost_completion(input, input.len(), dir.path(), None),
        Some("a alpha-new".into())
    );

    engine.environment.write().variable_state.alias.remove("a");
    assert_eq!(
        engine
            .convert_to_parsed_command_line(input, input.len())
            .command,
        "a"
    );
    let removed = engine
        .complete(input, input.len(), dir.path(), 30, None)
        .await;
    assert!(
        !removed
            .candidates
            .iter()
            .any(|candidate| candidate.text == "alpha-new")
    );
}

#[tokio::test]
async fn alias_wrapper_expansion_can_add_words_and_keeps_inline_value_range() {
    let engine = engine();
    alias(&engine, "apick", "pick --mode alpha 'fixed arg'");
    let dir = tempfile::tempdir().unwrap();
    let (input, cursor, parsed) = at(&engine, "sudo apick he§llo tail");
    let target = engine.dynamic_provider_target(&parsed);
    assert_eq!(target.command, "pick", "{target:?}");
    assert!(
        matches!(
            target.completion_context,
            CompletionContext::Argument { arg_index: 1, .. }
        ),
        "{target:?}"
    );
    let result = engine.complete(&input, cursor, dir.path(), 30, None).await;
    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "hello")
    );

    alias(&engine, "apick", "pick 'fixed arg'");
    let (input, cursor, _) = at(&engine, "sudo apick --mode=al§pha tail");
    let result = engine.complete(&input, cursor, dir.path(), 30, None).await;
    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "alpha")
    );
    let range = result.replacement_range.unwrap();
    assert_eq!(slice_chars(&input, range.start, range.end), "alpha");
    assert_eq!(
        replace_char_range(&input, range.start, range.end, "beta"),
        "sudo apick --mode=beta tail"
    );
}

#[test]
fn alias_fixed_quoted_git_directory_reaches_filesystem_context() {
    let engine = engine();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("repo space")).unwrap();
    alias(&engine, "ag", "git -C 'repo space' commit");
    for input in ["ag --ame", "sudo ag --ame"] {
        let context = engine
            .filesystem_context_for_input(input, input.len(), dir.path())
            .unwrap();
        assert_eq!(
            context.lookup_directory("", false),
            dir.path().join("repo space")
        );
    }
    let input = "ag > ";
    let context = engine
        .filesystem_context_for_input(input, input.len(), dir.path())
        .unwrap();
    assert_eq!(context.lookup_directory("", false), dir.path());
}
