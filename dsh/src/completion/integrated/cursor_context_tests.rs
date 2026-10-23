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
