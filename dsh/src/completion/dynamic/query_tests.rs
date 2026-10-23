use super::*;

#[test]
fn cached_queries_decode_source_once_and_preserve_values() {
    let env = Environment::new_synthetic();
    env.write().variable_state.paths.clear();
    let p = DynamicCompletionProvider::new(env);
    let dir = tempfile::tempdir().unwrap();
    let scope = p.cached_project_root(dir.path());
    let values: Vec<String> = [
        "feature/main",
        "main",
        "hello world",
        r#"quote"name"#,
        r"back\slash",
        "界é-value",
        "a;b",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    for kind in [
        DynamicCommandCacheKind::GitBranch,
        DynamicCommandCacheKind::CommandValue {
            command: "git".into(),
            value_kind: "revision".into(),
        },
        DynamicCommandCacheKind::CommandValue {
            command: "tmux".into(),
            value_kind: "session".into(),
        },
    ] {
        p.cache.write().commands.insert(
            DynamicCommandCacheKey {
                kind,
                scope_dir: scope.clone(),
            },
            CommandValueCacheEntry {
                values: values.clone(),
                cached_at: Instant::now(),
                last_load_duration: None,
                last_error: None,
            },
        );
    }
    for provider in ["git.branch", "git.revision", "tmux.session"] {
        for (source, expected) in [
            ("fe", "feature/main"),
            ("\"fe", "feature/main"),
            ("'fe", "feature/main"),
            ("f\"\"e", "feature/main"),
            (r"hello\ ", "hello world"),
            (r#"quote\"n"#, r#"quote"name"#),
            (r"back\\s", r"back\slash"),
            ("\"界é", "界é-value"),
            (r"a\;", "a;b"),
        ] {
            let input = format!("probe {source}");
            let parsed =
                crate::completion::parser::CommandLineParser::new().parse(&input, input.len());
            let result = p.collect_declared_dynamic_candidates(
                provider,
                None,
                &parsed,
                dir.path(),
                CachePolicy::CachedOnly,
            );
            assert!(
                result.iter().any(|c| c.text == expected),
                "{provider} {source:?}: {result:?}"
            );
            assert_eq!(parsed.current_token, source);
        }
    }
    assert!(!p.has_pending_refresh());
    assert!(
        p.cache
            .read()
            .commands
            .values()
            .all(|entry| entry.values == values)
    );
}
