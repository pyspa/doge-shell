use super::*;
use crate::completion::generator::CompletionGenerator;
use crate::completion::parser::CompletionContext;

fn engine() -> IntegratedCompletionEngine {
    let env = Environment::new_synthetic();
    env.write().variable_state.paths.clear();
    let e = IntegratedCompletionEngine::new(env);
    for json in [
        include_str!("../../../../completions/go.json"),
        include_str!("../../../../completions/git.json"),
        include_str!("../../../../completions/sudo.json"),
        include_str!("../../../../completions/env.json"),
    ] {
        e.command_completion
            .lock()
            .add_command(serde_json::from_str(json).unwrap());
    }
    e
}
fn parsed(e: &IntegratedCompletionEngine, marked: &str) -> ParsedCommandLine {
    let cursor = marked[..marked.find('§').unwrap()].chars().count();
    e.convert_to_parsed_command_line(&marked.replace('§', ""), cursor)
}
#[tokio::test]
async fn cache_does_not_cross_subcommand_boundary() {
    let e = engine();
    let cwd = tempfile::tempdir().unwrap();
    e.complete("go ", 3, cwd.path(), 50, None).await;
    let result = e.complete("go mod v", 8, cwd.path(), 50, None).await;
    let texts: Vec<_> = result.candidates.iter().map(|c| c.text.as_str()).collect();
    assert!(
        texts.contains(&"vendor") && texts.contains(&"verify"),
        "{texts:?}"
    );
    assert!(
        !texts.contains(&"version") && !texts.contains(&"vet"),
        "{texts:?}"
    );
}
#[test]
fn cursor_role_ignores_suffix_tokens() {
    let e = engine();
    for marked in [
        "git che§ckout branch",
        "git checkout§ branch",
        "git§ checkout branch",
        "git remote a§dd origin",
        "git che§ckout checkout",
    ] {
        let p = parsed(&e, marked);
        let db = e.command_completion.lock();
        let candidates = CompletionGenerator::new(&db)
            .generate_candidates(&p)
            .unwrap();
        let expected = if marked.starts_with("git§") {
            "git"
        } else if marked.contains("remote") {
            "add"
        } else {
            "checkout"
        };
        assert!(
            candidates.iter().any(|c| c.text == expected),
            "{marked}: {p:?} {candidates:?}"
        );
    }
}

fn choices(e: &IntegratedCompletionEngine) {
    e.command_completion.lock().add_command(serde_json::from_value(serde_json::json!({
        "command":"pick", "subcommands":[{"name":"other"}],
        "global_options":[{"long":"--mode","takes_value":true,"value_type":{"type":"Choice","data":["alpha","beta"]}}],
        "arguments":[{"name":"first","type":{"type":"Choice","data":["hello world","hello","界éalpha"]}},{"name":"second","type":{"type":"Choice","data":["second-only"]}}]
    })).unwrap());
}
#[test]
fn literal_query_keeps_raw_source_and_matches_words() {
    let e = engine();
    choices(&e);
    for (marked, expected) in [
        ("pick \"he§llo\" tail", "hello"),
        ("pick 'he§llo' tail", "hello"),
        (r"pick hello\ §world tail", "hello world"),
        ("pick he\"l\"§lo tail", "hello"),
        ("pick \"界é§alpha\" tail", "界éalpha"),
        ("pick --mode \"al§pha\" tail", "alpha"),
        ("pick --mode 'al§pha' tail", "alpha"),
    ] {
        let p = parsed(&e, marked);
        let raw = p.current_token.clone();
        let db = e.command_completion.lock();
        let result = CompletionGenerator::new(&db)
            .generate_candidates(&p)
            .unwrap();
        assert!(
            result.iter().any(|c| c.text == expected),
            "{marked}: {p:?} {result:?}"
        );
        assert_eq!(p.current_token, raw);
    }
}

#[test]
fn redirect_and_end_options_keep_argument_positions() {
    let e = engine();
    choices(&e);
    {
        let mut db = e.command_completion.lock();
        let mut def = db.get_command("pick").unwrap().clone();
        def.subcommands.clear();
        db.add_command(def);
    }
    for (marked, index) in [
        ("pick 2>&1 §", 0),
        ("pick first 2>&1 §", 1),
        ("pick -- > out §", 0),
        ("pick > out §", 0),
        ("pick -- --mode=§", 0),
        ("pick -- --mode §", 1),
    ] {
        let p = parsed(&e, marked);
        assert!(
            matches!(p.completion_context, CompletionContext::Argument{arg_index,..} if arg_index==index),
            "{marked}: {p:?}"
        );
    }
    let p = parsed(&e, "pick > §");
    assert!(
        matches!(
            p.completion_context,
            CompletionContext::Argument {
                arg_type: Some(ArgumentType::File { .. }),
                ..
            }
        ),
        "{p:?}"
    );
}
#[test]
fn wrappers_keep_command_identity_and_cursor() {
    let e = engine();
    for marked in [
        "sudo -n git che§ckout",
        "sudo -u root git che§ckout branch",
        "env KEY=value git che§ckout",
        "env -u KEY KEY=value git che§ckout branch",
    ] {
        let p = parsed(&e, marked);
        let target = e.dynamic_provider_target(&p);
        assert_eq!(target.command, "git", "{marked}: {p:?} -> {target:?}");
        assert_eq!(target.current_token, "che");
        assert_eq!(target.completion_context, CompletionContext::SubCommand);
        let db = e.command_completion.lock();
        let result = CompletionGenerator::new(&db)
            .generate_candidates(&p)
            .unwrap();
        assert!(
            result.iter().any(|c| c.text == "checkout"),
            "{marked}: {result:?}"
        );
    }
}

#[test]
fn concatenated_quotes_insert_one_complete_literal_word() {
    use crate::completion::{display::Candidate, shell_path::format_candidates_for_token};
    use crate::parser::{Rule, ShellParser};
    use pest::Parser;
    use skim::SkimItem;
    for raw in [r#""he"llo"#, "'he'llo", r#"he"l"lo"#, r#""he"'llo'"#] {
        let candidates =
            format_candidates_for_token(vec![Candidate::Basic("hello world".into())], Some(raw));
        let inserted = candidates[0].output().to_string();
        let pair = ShellParser::parse(Rule::span, &inserted)
            .unwrap()
            .next()
            .unwrap();
        assert_eq!(pair.as_span().end(), inserted.len(), "{raw}: {inserted}");
        assert_eq!(
            crate::parser::get_string(pair).as_deref(),
            Some("hello world")
        );
    }
}

fn job_engine() -> IntegratedCompletionEngine {
    let e = engine();
    e.command_completion.lock().add_command(
        serde_json::from_str(include_str!("../../../../completions/bg.json")).unwrap(),
    );
    e.set_shell_jobs(vec![(1, "fixture-job".into(), "Stopped".into())]);
    e
}

#[tokio::test]
async fn descriptor_targets_and_operator_interiors_are_suppressed() {
    let e = job_engine();
    let cwd = tempfile::tempdir().unwrap();
    for marked in [
        "bg 2>&12345§",
        "bg 2>&§1",
        "bg 2<&0§",
        "unknown 2>&1§",
        "echo 日 && bg 2>&1§; echo next",
        "bg 2>§&1",
        "bg >§>out",
    ] {
        let cursor = marked[..marked.find('§').unwrap()].chars().count();
        let input = marked.replace('§', "");
        let result = e.complete(&input, cursor, cwd.path(), 20, None).await;
        assert!(
            matches!(result.disposition, CompletionDisposition::Suppressed),
            "{marked}: {result:?}"
        );
        assert!(token_range_at_cursor(&input, cursor).is_none(), "{marked}");
        let p = parsed(&e, marked);
        let db = e.command_completion.lock();
        assert!(
            CompletionGenerator::new(&db)
                .generate_candidates(&p)
                .unwrap()
                .is_empty(),
            "suppressed generator: {marked}"
        );
        drop(db);
        assert!(
            e.ghost_completion(&input, cursor, cwd.path(), None)
                .is_none()
        );
    }
    for input in ["bg '2>&1'", r"bg 2\>\&1"] {
        let result = e
            .complete(input, input.chars().count(), cwd.path(), 20, None)
            .await;
        assert!(
            matches!(result.disposition, CompletionDisposition::Allowed),
            "literal operator: {input}: {result:?}"
        );
    }
    let input = "bg 2>&1 ";
    assert!(
        e.complete(input, input.len(), cwd.path(), 20, None)
            .await
            .candidates
            .iter()
            .any(|c| c.text == "%1")
    );
}

#[test]
fn redirect_boundary_belongs_to_target_not_operator() {
    let e = job_engine();
    for marked in [
        "bg >§out tail",
        "bg >>§out tail",
        "echo 日 && bg >§out; echo next",
        "bg >§ out",
        "bg >§",
    ] {
        let cursor = marked[..marked.find('§').unwrap()].chars().count();
        let input = marked.replace('§', "");
        let p = parsed(&e, marked);
        assert_eq!(p.current_token, "", "{marked}: {p:?}");
        assert!(
            matches!(
                p.completion_context,
                CompletionContext::Argument {
                    arg_type: Some(ArgumentType::File { .. }),
                    ..
                }
            ),
            "{marked}: {p:?}"
        );
        let range = token_range_at_cursor(&input, cursor).unwrap();
        assert_eq!(range.start, cursor, "{marked}");
        assert_eq!(
            slice_chars(&input, range.start, range.end),
            if marked.contains("§out") { "out" } else { "" },
            "{marked}"
        );
    }
}

#[tokio::test]
async fn job_queries_decode_source_without_spawning_providers() {
    let e = job_engine();
    let cwd = tempfile::tempdir().unwrap();
    for marked in [
        "bg \"%§\"",
        "bg '%§'",
        r"bg \%§",
        "echo 日 && bg \"%§x\" tail",
        "bg %\"\"§",
    ] {
        let cursor = marked[..marked.find('§').unwrap()].chars().count();
        let input = marked.replace('§', "");
        let result = e.complete(&input, cursor, cwd.path(), 20, None).await;
        assert!(
            result.candidates.iter().any(|c| c.text == "%1"),
            "{marked}: {result:?}"
        );
    }
    for (input, expected) in [("bg \"%", "bg \"%+"), ("bg '%", "bg '%+")] {
        assert_eq!(
            e.ghost_completion(input, input.chars().count(), cwd.path(), None)
                .as_deref(),
            Some(expected)
        );
    }
    assert!(!e.dynamic.has_pending_refresh());
}

#[tokio::test]
async fn cached_dynamic_queries_follow_cursor_and_ghost() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let git = dir.path().join("git");
    std::fs::write(&git, "#!/bin/sh\nprintf 'feature/main\\nmain\\n'\n").unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = Environment::new_synthetic();
    env.write().variable_state.paths = vec![dir.path().to_string_lossy().into_owned()];
    let mut e = IntegratedCompletionEngine::new(env);
    e.initialize_command_completion().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    e.set_notifier(tx);
    let warm = "git restore -s fe";
    loop {
        let result = e.complete(warm, warm.len(), dir.path(), 30, None).await;
        if result.candidates.iter().any(|c| c.text == "feature/main") {
            break;
        }
        tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .unwrap()
            .unwrap();
    }
    for marked in [
        "git restore -s \"fe§ature/other\" tail",
        "git restore -s 'fe§ature/other' tail",
        "git restore --source=\"fe§ature/other\" tail",
        "echo 日 && sudo -n git restore -s \"fe§ature/other\" tail",
        "git restore -s f\"\"e§ature/other tail",
    ] {
        let cursor = marked[..marked.find('§').unwrap()].chars().count();
        let input = marked.replace('§', "");
        let result = e.complete(&input, cursor, dir.path(), 30, None).await;
        assert!(
            result.candidates.iter().any(|c| c.text == "feature/main"),
            "{marked}: {result:?}"
        );
        let range = result.replacement_range.unwrap();
        assert!(slice_chars(&input, range.start, range.end).contains("ature/other"));
    }
    let generation = e.dynamic.refresh_generation();
    let input = "git restore -s \"fe";
    assert_eq!(
        e.ghost_completion(input, input.len(), dir.path(), None)
            .as_deref(),
        Some("git restore -s \"feature/main")
    );
    assert_eq!(
        e.dynamic.refresh_generation(),
        generation,
        "ghost must not schedule subprocess work"
    );
}
