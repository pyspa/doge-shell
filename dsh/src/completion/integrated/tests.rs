use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

fn top_level_cache_lookup(
    engine: &IntegratedCompletionEngine,
    input: &str,
) -> Option<crate::completion::cache::CacheLookup<EnhancedCandidate>> {
    let scope = engine.current_path_cache_scope();
    engine.cache.lookup_scoped(scope, input)
}

async fn wait_for_candidate(
    engine: &IntegratedCompletionEngine,
    input: &str,
    cwd: &Path,
    expected: &str,
) -> CompletionResult {
    let start = std::time::Instant::now();
    loop {
        let result = engine.complete(input, input.len(), cwd, 50, None).await;
        if result
            .candidates
            .iter()
            .any(|candidate| candidate.text == expected)
        {
            return result;
        }
        let last_candidates: Vec<_> = result
            .candidates
            .iter()
            .map(|candidate| candidate.text.clone())
            .collect();
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for completion candidate {expected} for input {input}; last candidates: {last_candidates:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Wait until a completion result both shows `expected` and has been published
/// as an exact top-level cache entry containing `expected`.
///
/// A refresh finishing mid-request can make the dynamic candidate visible in a
/// result that started while the refresh was still pending. Such a request
/// stays non-cacheable for its lifetime (see `complete()`), so "candidate
/// visible" and "top-level cache published" are two separate conditions that
/// must both hold. Synchronization uses the existing completion refresh
/// notifier instead of arbitrary sleeps: each iteration re-checks state and
/// otherwise waits for the next refresh notification under a deadline.
async fn wait_for_candidate_and_exact_cache(
    engine: &IntegratedCompletionEngine,
    notifications: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
    input: &str,
    cwd: &Path,
    expected: &str,
) -> CompletionResult {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let result = engine.complete(input, input.len(), cwd, 50, None).await;
        let candidate_ready = result
            .candidates
            .iter()
            .any(|candidate| candidate.text == expected);
        let cached = top_level_cache_lookup(engine, input);
        let cache_ready = cached.as_ref().is_some_and(|cached| {
            cached.exact
                && cached
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == expected)
        });
        if candidate_ready && cache_ready {
            return result;
        }

        let now = Instant::now();
        let last_candidates: Vec<_> = result
            .candidates
            .iter()
            .map(|candidate| candidate.text.clone())
            .collect();
        let cached_summary = cached.as_ref().map(|cached| {
            (
                cached.exact,
                cached
                    .candidates
                    .iter()
                    .map(|candidate| candidate.text.clone())
                    .collect::<Vec<_>>(),
            )
        });
        assert!(
            now < deadline,
            "timed out waiting for completion to settle: input={input:?} expected={expected:?} \
             generation={} pending={} last_candidates={last_candidates:?} cached={cached_summary:?}",
            engine.dynamic.refresh_generation(),
            engine.dynamic.has_pending_refresh(),
        );

        let remaining = deadline.saturating_duration_since(now);
        tokio::time::timeout(remaining, notifications.recv())
            .await
            .expect("timed out waiting for dynamic completion refresh")
            .expect("completion notifier closed unexpectedly");
    }
}

fn write_executable_script(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn engine_with_path(bin_dir: &Path) -> IntegratedCompletionEngine {
    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![bin_dir.display().to_string()];
        env.clear_command_cache();
    }
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();
    engine
}

fn parsed_for(engine: &IntegratedCompletionEngine, input: &str) -> ParsedCommandLine {
    let mut parsed = CommandLineParser::new().parse(input, input.len());
    engine.normalize_parsed_command_line(&mut parsed);
    parsed
}

/// `dynamic/git.rs::collect_git_argument_candidates` hand-dispatches ~90
/// lines of `(subcommand, arg_index) -> provider` cases that
/// `completions/git.json` already declares as data. Before that Rust
/// table can be deleted, every case it handles must resolve to the
/// *same* declared provider through the ordinary JSON path - otherwise
/// deleting it silently drops a completion. This is what makes that
/// deletion safe: both paths end up calling the identical
/// `DynamicCompletionProvider::collect_git_*_candidates` method, so
/// matching providers means matching output, without needing a real
/// git repository to compare candidate lists against.
#[test]
fn every_hand_dispatched_git_argument_case_matches_the_declared_json_provider() {
    let mut engine = IntegratedCompletionEngine::new(Environment::new());
    engine.initialize_command_completion().unwrap();
    let cases: &[(&str, &str)] = &[
        ("git checkout ", "git.checkout_target"),
        ("git switch ", "git.branch"),
        ("git merge ", "git.branch"),
        ("git rebase ", "git.branch"),
        ("git add ", "git.changed_path"),
        ("git restore ", "git.changed_path"),
        ("git push origin ", "git.push_branch"),
        ("git push ", "git.remote"),
        ("git pull origin ", "git.remote_branch"),
        ("git pull ", "git.remote"),
        ("git fetch origin ", "git.remote_branch"),
        ("git fetch ", "git.remote"),
        ("git log ", "git.revision"),
        ("git diff ", "git.revision"),
        ("git show ", "git.revision"),
        ("git reset ", "git.revision"),
        ("git branch ", "git.branch"),
        ("git tag ", "git.tag"),
        ("git stash pop ", "git.stash"),
        ("git stash apply ", "git.stash"),
        ("git stash drop ", "git.stash"),
        ("git remote remove ", "git.remote"),
        ("git remote rename ", "git.remote"),
        ("git remote show ", "git.remote"),
        ("git remote get-url ", "git.remote"),
        ("git remote set-url ", "git.remote"),
        ("git worktree remove ", "git.worktree"),
        ("git worktree move ", "git.worktree"),
        ("git worktree lock ", "git.worktree"),
        ("git worktree unlock ", "git.worktree"),
        ("git worktree repair ", "git.worktree"),
        ("git worktree add x ", "git.branch"),
        // Second-argument-position cases for the subcommands whose old
        // Rust dispatch never checked `arg_index`: add/restore accept
        // multiple paths, log/diff/show/reset multiple revisions, and
        // branch/tag were offered regardless of position even though a
        // second `git branch`/`git tag` argument does not always name
        // another branch/tag. `completions/git.json` now marks each of
        // these arguments `"multiple": true` to match. Without it,
        // `resolve_argument_definition` (this file) returns `None` past
        // the first argument and this second position silently loses
        // its dynamic completion - the regression this block guards.
        ("git add f1 ", "git.changed_path"),
        ("git restore f1 ", "git.changed_path"),
        ("git log v1 ", "git.revision"),
        ("git diff v1 ", "git.revision"),
        ("git show v1 ", "git.revision"),
        ("git reset v1 ", "git.revision"),
        ("git branch b1 ", "git.branch"),
        ("git tag t1 ", "git.tag"),
        // `git restore -s/--source <TAB>` is an OptionValue context, not
        // an Argument one, but it too is already declared in
        // completions/git.json (on the option itself) - proving this
        // resolves via JSON is what let collect_git_candidates drop its
        // own hand-written special case for it.
        ("git restore -s ", "git.revision"),
        ("git restore --source ", "git.revision"),
    ];

    for (input, expected_provider) in cases {
        let parsed = parsed_for(&engine, input);
        match engine.argument_type_for_completion_context(&parsed) {
            Some(ArgumentType::Dynamic { provider, .. }) => {
                assert_eq!(
                    provider, *expected_provider,
                    "{input:?} resolved to the wrong provider through completions/git.json"
                );
            }
            other => panic!(
                "{input:?} did not resolve to a Dynamic provider via completions/git.json \
                     (got {other:?}); collect_git_argument_candidates cannot be deleted until it does"
            ),
        }
    }
}

/// A skill's name is chosen by the model, so `skill remove <TAB>` not
/// offering it meant reading `skill list` first every time.
#[tokio::test]
async fn skill_subcommands_complete_project_skill_names() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    let skill = root.join(".dogesh/skills/deploy-staging");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: deploy-staging\ndescription: repo deploy steps\n---\n",
    )
    .unwrap();

    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let engine = engine_with_path(&bin);

    for line in [
        "skill show ",
        "skill path ",
        "skill remove ",
        "skill archive ",
        "skill unarchive ",
        "skill pin ",
        "skill unpin ",
    ] {
        wait_for_candidate(&engine, line, root, "deploy-staging").await;
    }

    // Not every argument position: `skill list` takes none.
    let listed = engine.complete("skill list ", 11, root, 50, None).await;
    assert!(
        !listed
            .candidates
            .iter()
            .any(|candidate| candidate.text == "deploy-staging"),
        "{:?}",
        listed.candidates
    );
}

/// `skill diff|approve|reject <TAB>` completes a pending proposal id,
/// never a skill name - offering the wrong kind of token there would be
/// actively misleading.
///
/// No other test in this crate's unit-test binary reads or writes
/// `XDG_STATE_HOME` in-process (the integration tests under
/// `dsh/tests/` set it only for a spawned child process), so this one
/// does not need to share a lock with anything else.
#[tokio::test]
async fn skill_review_subcommands_complete_pending_proposal_ids() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    let state = tempdir().unwrap();

    let previous = std::env::var_os("XDG_STATE_HOME");
    // SAFETY: this is the only test in this binary that touches
    // `XDG_STATE_HOME` in-process.
    unsafe { std::env::set_var("XDG_STATE_HOME", state.path()) };

    // Written directly rather than through `dsh-builtin`'s own
    // `pending::stage`, which is crate-private: this only needs the
    // file on disk in the shape `pending_proposal_ids()` reads back.
    let pending_dir = state.path().join("dogesh/skills-pending");
    fs::create_dir_all(&pending_dir).unwrap();
    fs::write(
        pending_dir.join("project.deploy-staging.json"),
        serde_json::json!({
            "version": 1,
            "id": "project.deploy-staging",
            "scope": "project",
            "name": "deploy-staging",
            "file": "SKILL.md",
            "action": "create",
            "project_root": root.join(".dogesh/skills"),
            "contents": "---\nname: deploy-staging\ndescription: d\n---\n",
            "base_digest": null,
            "created_ms": 1,
            "origin": "tool",
            "note": null,
        })
        .to_string(),
    )
    .unwrap();

    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let engine = engine_with_path(&bin);

    for line in ["skill diff ", "skill approve ", "skill reject "] {
        wait_for_candidate(&engine, line, root, "project.deploy-staging").await;
    }

    match previous {
        Some(value) => unsafe { std::env::set_var("XDG_STATE_HOME", value) },
        None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
    }
}

#[test]
fn test_integrated_completion_engine_creation() {
    let engine = IntegratedCompletionEngine::new(Environment::new());
    assert!(engine.loader.is_none());
}

#[test]
fn test_deduplicate_and_sort_merges_trailing_slash_duplicate() {
    // The JSON/FileSystemGenerator stage yields directories without a
    // trailing separator (higher priority); the fish-fallback stage
    // yields the same directory with one (lower priority). Both should
    // collapse into a single candidate, keeping the higher-priority one.
    let engine = IntegratedCompletionEngine::new(Environment::new());
    let candidates = vec![
        EnhancedCandidate {
            text: "src/".to_string(),
            description: None,
            candidate_type: CandidateType::Directory,
            priority: 35,
        },
        EnhancedCandidate {
            text: "src".to_string(),
            description: None,
            candidate_type: CandidateType::Directory,
            priority: 50,
        },
    ];

    let result = engine.deduplicate_and_sort(candidates, 50, None, None);

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].text, "src");
}

/// `collect_project_candidates` carries its most-recently-used order in
/// `priority`, because this sort is the only thing that decides what the user
/// sees and its last tiebreak is the candidate text. If candidates that share a
/// priority and a type stopped coming back alphabetical the encoding would be
/// unnecessary -- and if descending priorities stopped being honoured, `pj
/// <TAB>` would silently go back to alphabetical with nothing to notice it.
#[test]
fn descending_priority_outranks_the_alphabetical_tiebreak() {
    let engine = IntegratedCompletionEngine::new(Environment::new());
    let argument = |text: &str, priority: u32| EnhancedCandidate {
        text: text.to_string(),
        description: None,
        candidate_type: CandidateType::Argument,
        priority,
    };

    // Equal priority: the text tiebreak decides, which is the behaviour that
    // made a plain recency-ordered Vec useless.
    let flat = engine.deduplicate_and_sort(
        vec![argument("zulu", 90), argument("alpha", 90)],
        50,
        None,
        None,
    );
    assert_eq!(
        flat.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["alpha", "zulu"]
    );

    // Descending priority: the collection order survives instead.
    let ranked = engine.deduplicate_and_sort(
        vec![argument("zulu", 90), argument("alpha", 89)],
        50,
        None,
        None,
    );
    assert_eq!(
        ranked.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["zulu", "alpha"]
    );
}

#[test]
fn test_deduplicate_and_sort_keeps_unrelated_types_with_same_trimmed_text() {
    // A Directory candidate "src/" trims to the same text as an unrelated
    // candidate literally named "src" (e.g. a git branch or history
    // entry). They must not be merged just because they share a key once
    // the directory's trailing separator is stripped.
    let engine = IntegratedCompletionEngine::new(Environment::new());
    let candidates = vec![
        EnhancedCandidate {
            text: "src/".to_string(),
            description: None,
            candidate_type: CandidateType::Directory,
            priority: 50,
        },
        EnhancedCandidate {
            text: "src".to_string(),
            description: None,
            candidate_type: CandidateType::Argument,
            priority: 50,
        },
    ];

    let result = engine.deduplicate_and_sort(candidates, 50, None, None);

    assert_eq!(result.len(), 2);
}

#[tokio::test]
async fn shell_job_completion_tracks_live_job_snapshot() {
    let mut engine = IntegratedCompletionEngine::new(Environment::new());
    engine.initialize_command_completion().unwrap();
    engine.set_shell_jobs(vec![
        (1, "sleep 30".to_string(), "Running".to_string()),
        (2, "vim".to_string(), "Stopped".to_string()),
    ]);
    let dir = tempdir().unwrap();

    let result = engine.complete("fg %", 4, dir.path(), 50, None).await;
    let texts = result
        .candidates
        .iter()
        .map(|candidate| candidate.text.as_str())
        .collect::<Vec<_>>();
    for expected in ["%+", "%-", "%1", "%2"] {
        assert!(
            texts.contains(&expected),
            "missing job candidate {expected}: {texts:?}"
        );
    }

    engine.set_shell_jobs(vec![(3, "tail -f log".to_string(), "Running".to_string())]);
    let result = engine.complete("fg %", 4, dir.path(), 50, None).await;
    let texts = result
        .candidates
        .iter()
        .map(|candidate| candidate.text.as_str())
        .collect::<Vec<_>>();
    assert!(texts.contains(&"%+"));
    assert!(texts.contains(&"%3"));
    assert!(!texts.contains(&"%-"));
    assert!(!texts.contains(&"%1"));
    assert!(!texts.contains(&"%2"));
}

fn engine_with_variable(name: &str, value: &str) -> IntegratedCompletionEngine {
    let environment = Environment::new();
    environment
        .write()
        .variable_state
        .variables
        .insert(name.to_string(), value.to_string());
    IntegratedCompletionEngine::new(environment)
}

#[tokio::test]
async fn dollar_token_completes_shell_variables_with_prefix_kept() {
    let engine = engine_with_variable("DOGESH_SPECIAL_VAR", "1");
    let input = "echo $DOGESH_SPEC";
    let dir = tempdir().unwrap();
    let result = engine
        .complete(input, input.chars().count(), dir.path(), 50, None)
        .await;

    assert!(
        result
            .candidates
            .iter()
            .any(|c| c.text == "$DOGESH_SPECIAL_VAR"),
        "expected `$DOGESH_SPECIAL_VAR` among {:?}",
        result
            .candidates
            .iter()
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );
    // The replacement range must cover the whole `$...` token so the
    // inserted value replaces it (rather than appending after `$DOGESH_SPEC`).
    let range = result.replacement_range.expect("replacement range");
    let replaced: String = input
        .chars()
        .skip(range.start)
        .take(range.end - range.start)
        .collect();
    assert_eq!(replaced, "$DOGESH_SPEC");
}

#[tokio::test]
async fn brace_variable_token_completes_with_brace_form() {
    let engine = engine_with_variable("DOGESH_BRACE_VAR", "1");
    let input = "echo ${DOGESH_BRACE";
    let dir = tempdir().unwrap();
    let result = engine
        .complete(input, input.chars().count(), dir.path(), 50, None)
        .await;

    assert!(
        result
            .candidates
            .iter()
            .any(|c| c.text == "${DOGESH_BRACE_VAR}"),
        "expected `${{DOGESH_BRACE_VAR}}` among {:?}",
        result
            .candidates
            .iter()
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn tilde_token_completes_user_home() {
    let engine = IntegratedCompletionEngine::new(Environment::new());
    let input = "cat ~roo";
    let dir = tempdir().unwrap();
    let result = engine
        .complete(input, input.chars().count(), dir.path(), 50, None)
        .await;

    // `root` exists on Linux/macOS; candidate keeps the leading `~`.
    assert!(
        result.candidates.iter().any(|c| c.text == "~root"),
        "expected `~root` among {:?}",
        result
            .candidates
            .iter()
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );
}

#[test]
fn special_token_dispatch_ignores_normal_tokens() {
    let engine = IntegratedCompletionEngine::new(Environment::new());
    assert!(engine.collect_special_token_candidates("git").is_none());
    assert!(engine.collect_special_token_candidates("--flag").is_none());
    // A `$VAR/subpath` token is a path, not a bare variable.
    assert!(
        engine
            .collect_special_token_candidates("$HOME/foo")
            .is_none()
    );
    assert!(engine.collect_special_token_candidates("$").is_some());
}

#[test]
fn test_candidate_type_sorting() {
    let mut types = [
        CandidateType::File,
        CandidateType::SubCommand,
        CandidateType::LongOption,
        CandidateType::Directory,
    ];

    types.sort_by_key(|t| t.sort_order());

    assert_eq!(types[0], CandidateType::SubCommand);
    assert_eq!(types[1], CandidateType::LongOption);
    assert_eq!(types[2], CandidateType::Directory);
    assert_eq!(types[3], CandidateType::File);
}

#[test]
fn test_enhanced_candidate_creation() {
    let candidate = EnhancedCandidate {
        text: "test".to_string(),
        description: Some("Test command".to_string()),
        candidate_type: CandidateType::SubCommand,
        priority: 100,
    };

    assert_eq!(candidate.text, "test");
    assert_eq!(candidate.candidate_type.icon(), "⚡");
    assert_eq!(candidate.candidate_type.sort_order(), 1);
}

#[test]
fn test_enhanced_candidate_to_candidate_conversion() {
    let enhanced_candidate = EnhancedCandidate {
        text: "commit".to_string(),
        description: Some("Record changes to the repository".to_string()),
        candidate_type: CandidateType::SubCommand,
        priority: 100,
    };

    let candidate = enhanced_candidate.to_candidate();

    match candidate {
        Candidate::Command { name, description } => {
            assert_eq!(name, "commit");
            assert_eq!(description, "Record changes to the repository");
        }
        _ => panic!("Expected Command candidate"),
    }
}

#[test]
fn token_range_at_cursor_preserves_double_quoted_spaces() {
    let input = r#"cat "dir with space/foo"#;
    let cursor_before_last_o = r#"cat "dir with space/fo"#.chars().count();

    assert_eq!(
        token_range_at_cursor(input, cursor_before_last_o),
        Some(CompletionReplacementRange { start: 4, end: 23 })
    );
    assert_eq!(
        slice_chars(input, 4, 23),
        r#""dir with space/foo"#.to_string()
    );
}

#[test]
fn token_range_at_cursor_preserves_backslash_escaped_spaces() {
    let input = r#"cat dir\ with\ space/foo"#;
    let cursor_before_last_o = r#"cat dir\ with\ space/fo"#.chars().count();

    assert_eq!(
        token_range_at_cursor(input, cursor_before_last_o),
        Some(CompletionReplacementRange { start: 4, end: 24 })
    );
    assert_eq!(
        slice_chars(input, 4, 24),
        r#"dir\ with\ space/foo"#.to_string()
    );
}

#[test]
fn shell_token_range_supplies_raw_token_for_path_formatting() {
    let input = r#"cat "dir with space/fo"#;
    let range = token_range_at_cursor(input, input.chars().count()).unwrap();
    let raw_token = slice_chars(input, range.start, range.end);
    let candidates = vec![Candidate::File {
        path: "dir with space/foo".to_string(),
        is_dir: false,
    }];

    let formatted =
        crate::completion::shell_path::format_candidates_for_token(candidates, Some(&raw_token));

    assert_eq!(
        formatted[0],
        Candidate::File {
            path: r#""dir with space/foo"#.to_string(),
            is_dir: false,
        }
    );
}

#[tokio::test]
async fn quoted_path_completion_generates_and_formats_candidate() {
    let dir = tempdir().unwrap();
    let spaced_dir = dir.path().join("dir with space");
    fs::create_dir(&spaced_dir).unwrap();
    fs::write(spaced_dir.join("foo"), "").unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = format!(r#"cat "{}/fo"#, spaced_dir.display());
    let result = engine
        .complete(&input, input.chars().count(), dir.path(), 50, None)
        .await;
    let expected = spaced_dir.join("foo").to_string_lossy().to_string();

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == expected),
        "expected normalized file candidate {:?} in {:?}",
        expected,
        result.candidates
    );

    let range = result.replacement_range.expect("replacement range");
    let raw_token = slice_chars(&input, range.start, range.end);
    let formatted = crate::completion::shell_path::format_candidates_for_token(
        engine.to_candidates(result.candidates),
        Some(&raw_token),
    );

    assert!(
        formatted.iter().any(|candidate| {
            matches!(
                candidate,
                Candidate::File { path, is_dir: false }
                    if path == &format!("\"{expected}")
            )
        }),
        "expected quoted display candidate in {:?}",
        formatted
    );
}

#[tokio::test]
async fn escaped_path_completion_generates_and_formats_candidate() {
    let dir = tempdir().unwrap();
    let spaced_dir = dir.path().join("dir with space");
    fs::create_dir(&spaced_dir).unwrap();
    fs::write(spaced_dir.join("foo"), "").unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let escaped_prefix = spaced_dir
        .join("fo")
        .to_string_lossy()
        .replace(' ', r#"\ "#);
    let input = format!("cat {escaped_prefix}");
    let result = engine
        .complete(&input, input.chars().count(), dir.path(), 50, None)
        .await;
    let expected = spaced_dir.join("foo").to_string_lossy().to_string();

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == expected),
        "expected normalized file candidate {:?} in {:?}",
        expected,
        result.candidates
    );

    let range = result.replacement_range.expect("replacement range");
    let raw_token = slice_chars(&input, range.start, range.end);
    let formatted = crate::completion::shell_path::format_candidates_for_token(
        engine.to_candidates(result.candidates),
        Some(&raw_token),
    );
    let escaped_expected = expected.replace(' ', r#"\ "#);

    assert!(
        formatted.iter().any(|candidate| {
            matches!(
                candidate,
                Candidate::File { path, is_dir: false } if path == &escaped_expected
            )
        }),
        "expected escaped display candidate in {:?}",
        formatted
    );
}

#[tokio::test]
async fn pytest_string_option_value_does_not_fallback_to_files_but_next_arg_does() {
    let dir = tempdir().unwrap();
    let test_file = dir.path().join("tests_alpha.py");
    fs::write(&test_file, "").unwrap();
    let file_prefix = dir.path().join("tests_").to_string_lossy().to_string();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let option_value_input = format!("pytest -k {file_prefix}");
    let option_value_result = engine
        .complete(
            &option_value_input,
            option_value_input.len(),
            dir.path(),
            50,
            None,
        )
        .await;
    assert!(
        !option_value_result
            .candidates
            .iter()
            .any(|candidate| candidate.text == test_file.to_string_lossy()),
        "String option values must not fallback to file candidates: {:?}",
        option_value_result.candidates
    );

    let positional_input = format!("pytest -k expr {file_prefix}");
    let positional_result = engine
        .complete(
            &positional_input,
            positional_input.len(),
            dir.path(),
            50,
            None,
        )
        .await;
    assert!(
        positional_result
            .candidates
            .iter()
            .any(|candidate| candidate.text == test_file.to_string_lossy()),
        "positional pytest arguments should still complete files: {:?}",
        positional_result.candidates
    );
}

#[tokio::test]
async fn double_dash_allows_following_file_argument_completion() {
    let dir = tempdir().unwrap();
    let test_file = dir.path().join("alpha.py");
    fs::write(&test_file, "").unwrap();
    let file_prefix = dir.path().join("alp").to_string_lossy().to_string();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = format!("pytest -- {file_prefix}");
    let result = engine
        .complete(&input, input.len(), dir.path(), 50, None)
        .await;

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == test_file.to_string_lossy()),
        "expected file after -- to complete as positional argument: {:?}",
        result.candidates
    );
}

#[tokio::test]
async fn json_completion_filters_by_current_token() {
    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine
        .initialize_command_completion()
        .expect("command completion should initialize");

    let input = "git a";
    let cursor_pos = input.len();
    let current_dir = std::env::current_dir().expect("current dir available");

    let completion_result = engine
        .complete(input, cursor_pos, &current_dir, 50, None)
        .await;

    assert!(
        !completion_result.candidates.is_empty(),
        "expected git subcommand suggestions"
    );

    for candidate in &completion_result.candidates {
        assert!(
            candidate.text.starts_with('a'),
            "candidate '{}' should be filtered by prefix",
            candidate.text
        );
    }

    assert!(
        completion_result.candidates.iter().any(|c| c.text == "add"),
        "git add should remain available"
    );

    assert_eq!(
        completion_result.framework,
        crate::completion::framework::CompletionFrameworkKind::Skim
    );
}

#[tokio::test]
async fn dynamic_command_static_subcommand_caches_after_refresh_settles() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(
        &bin_dir.join("git"),
        "#!/bin/sh\nif [ \"$1\" = \"config\" ]; then printf 'alias.cheat status\\n'; fi\n",
    );
    let engine = engine_with_path(&bin_dir);
    // Connect the refresh notifier before the cold request so the worker
    // completion event is queued instead of racing receiver registration.
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel();
    engine.set_notifier(notify_tx);

    let input = "git che";
    let first = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    assert!(
        first
            .candidates
            .iter()
            .any(|candidate| candidate.text == "checkout"),
        "expected git checkout from JSON subcommand completion"
    );
    assert!(
        !first
            .candidates
            .iter()
            .any(|candidate| candidate.text == "cheat"),
        "cold result must not publish dynamic alias data before refresh completion"
    );
    assert!(
        top_level_cache_lookup(&engine, input).is_none(),
        "a cold dynamic refresh must keep the partial result out of the top-level cache"
    );

    // A refresh finishing mid-request can make "cheat" visible in a result
    // that remains non-cacheable for its lifetime, so wait for the settled
    // state where both the candidate and its exact top-level cache entry are
    // observable.
    let second =
        wait_for_candidate_and_exact_cache(&engine, &mut notify_rx, input, dir.path(), "cheat")
            .await;
    let cached = top_level_cache_lookup(&engine, input)
        .expect("the complete result should become cacheable after the dynamic refresh settles");
    assert!(cached.exact);
    assert!(
        cached
            .candidates
            .iter()
            .any(|candidate| candidate.text == "cheat")
    );

    let second_texts = second
        .candidates
        .iter()
        .map(|candidate| candidate.text.as_str())
        .collect::<Vec<_>>();
    assert!(second_texts.contains(&"checkout"));
    assert!(second_texts.contains(&"cheat"));
}

#[tokio::test]
async fn dynamic_command_argument_does_not_use_completion_cache() {
    let dir = tempdir().unwrap();

    std::process::Command::new("git")
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    fs::write(dir.path().join("README.md"), "hello\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "README.md"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["checkout", "-b", "feature/test-branch"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.clear_command_cache();
    }
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "git checkout feat";
    let _ = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    let _ = wait_for_candidate(&engine, input, dir.path(), "feature/test-branch").await;

    assert!(
        top_level_cache_lookup(&engine, input).is_none(),
        "dynamic argument values must stay out of the top-level completion cache"
    );
}

#[tokio::test]
async fn system_command_top_level_cache_is_path_generation_scoped() {
    let dir = tempdir().unwrap();
    let path_a = dir.path().join("a");
    let path_b = dir.path().join("b");
    fs::create_dir(&path_a).unwrap();
    fs::create_dir(&path_b).unwrap();
    write_executable_script(&path_a.join("zz-old"), "#!/bin/sh\nexit 0\n");
    write_executable_script(&path_b.join("zz-new"), "#!/bin/sh\nexit 0\n");

    let engine = engine_with_path(&path_a);
    let input = "sudo command zz-";
    let scope_a = engine.current_path_cache_scope();
    let first = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    assert!(
        first
            .candidates
            .iter()
            .any(|candidate| candidate.text == "zz-old")
    );
    let cached = engine
        .cache
        .lookup_scoped(scope_a, input)
        .expect("same-generation system result should use the top-level cache");
    assert!(
        cached
            .candidates
            .iter()
            .any(|candidate| candidate.text == "zz-old"),
        "same-generation system result should remain cached: {cached:?}"
    );

    engine
        .environment
        .write()
        .set_and_export_shell_var("PATH".to_string(), path_b.display().to_string());
    let scope_b = engine.current_path_cache_scope();
    assert!(
        engine.cache.lookup_scoped(scope_b, input).is_none(),
        "PATH switch must not reuse the previous generation's top-level entry"
    );
    let second = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    assert!(
        second
            .candidates
            .iter()
            .any(|candidate| candidate.text == "zz-new")
    );
    assert!(
        !second
            .candidates
            .iter()
            .any(|candidate| candidate.text == "zz-old"),
        "top-level completion cache resurfaced the previous PATH: {:?}",
        second.candidates
    );
    let cached = engine
        .cache
        .lookup_scoped(scope_b, input)
        .expect("new PATH generation should publish its own top-level entry");
    assert!(
        cached
            .candidates
            .iter()
            .any(|candidate| candidate.text == "zz-new"),
        "new generation cache should contain only its PATH snapshot: {cached:?}"
    );

    engine
        .environment
        .write()
        .set_and_export_shell_var("PATH".to_string(), path_a.display().to_string());
    let scope_a2 = engine.current_path_cache_scope();
    assert!(
        engine.cache.lookup_scoped(scope_a2, input).is_none(),
        "A -> B -> A must still allocate a fresh top-level cache scope"
    );
    let third = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    assert!(
        third
            .candidates
            .iter()
            .any(|candidate| candidate.text == "zz-old")
            && !third
                .candidates
                .iter()
                .any(|candidate| candidate.text == "zz-new"),
        "A -> B -> A reused the wrong generation: {:?}",
        third.candidates
    );
}

#[tokio::test]
async fn git_dynamic_value_candidates_skip_fallback_collectors() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(
        &bin_dir.join("git"),
        "#!/bin/sh\nif [ \"$1\" = \"for-each-ref\" ]; then printf 'feature/probe\\nmain\\n'; fi\n",
    );

    let external_marker = dir.path().join("external-called");
    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![bin_dir.display().to_string()];
        env.variable_state.variables.insert(
            "DOGESH_EXTERNAL_COMPLETER".to_string(),
            format!(
                "printf 'external-branch\\tExternal completer\\n'; printf called > {}",
                external_marker.display()
            ),
        );
        env.clear_command_cache();
    }
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "git checkout feat";
    let result = wait_for_candidate(&engine, input, dir.path(), "feature/probe").await;

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "feature/probe"),
        "expected git checkout target from dynamic completion"
    );
    assert!(
        !result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "external-branch"),
        "git dynamic value completion should not merge external fallback candidates"
    );
    assert!(
        !external_marker.exists(),
        "external fallback should not run for exclusive git dynamic value completion"
    );
    assert!(
        top_level_cache_lookup(&engine, input).is_none(),
        "exclusive dynamic argument values must stay out of the top-level completion cache"
    );
}

#[test]
fn git_subcommand_accepts_paths_detects_checkout_and_restore() {
    let make = |command: &str, sub: &str| parser::ParsedCommandLine {
        command: command.to_string(),
        subcommand_path: vec![sub.to_string()],
        raw_args: vec![],
        args: vec![],
        options: vec![],
        current_token: "feat".to_string(),
        current_arg: Some("feat".to_string()),
        completion_context: parser::CompletionContext::Argument {
            arg_index: 0,
            arg_type: None,
        },
        specified_options: vec![],
        specified_arguments: vec![],
        cursor_index: 0,
    };
    assert!(git_subcommand_accepts_paths(&make("git", "checkout")));
    assert!(git_subcommand_accepts_paths(&make("git", "restore")));
    // Branch-only subcommands must NOT pull in file candidates.
    assert!(!git_subcommand_accepts_paths(&make("git", "switch")));
    assert!(!git_subcommand_accepts_paths(&make("git", "branch")));
    // Non-git commands are unaffected.
    assert!(!git_subcommand_accepts_paths(&make("ls", "checkout")));
}

#[tokio::test]
async fn git_checkout_is_not_exclusive_against_files() {
    // Regression: `git checkout` used to be exclusive with a branch-only
    // dynamic provider, which suppressed file completion entirely. Files
    // must still be offered. We use an absolute-path token (independent of
    // the process CWD) so the working-tree file is found deterministically.
    let dir = tempdir().unwrap();
    let git_env = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap();
    };
    git_env(&["init"]);
    git_env(&["config", "user.email", "test@example.com"]);
    git_env(&["config", "user.name", "Test User"]);
    fs::write(dir.path().join("README.md"), "hello\n").unwrap();
    git_env(&["add", "README.md"]);
    git_env(&["commit", "-m", "init"]);
    git_env(&["checkout", "-b", "feature/test-branch"]);
    fs::write(dir.path().join("features.txt"), "x\n").unwrap();

    let environment = Environment::new();
    environment.write().clear_command_cache();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let token_prefix = format!("{}/feat", dir.path().display());
    let input = format!("git checkout {token_prefix}");
    let result = engine
        .complete(&input, input.chars().count(), dir.path(), 50, None)
        .await;

    assert!(
        result
            .candidates
            .iter()
            .any(|c| c.text.contains("features")),
        "expected working-tree file `features.txt` among {:?}",
        result
            .candidates
            .iter()
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );
}

#[test]
fn history_boost_skips_file_candidates() {
    let environment = Environment::new();
    let engine = IntegratedCompletionEngine::new(environment);

    let mut history = crate::history::History::new();
    history.add_test_entry("bar.txt");
    history.add_test_entry("bar");
    let history_arc = std::sync::Arc::new(parking_lot::Mutex::new(history));

    let candidates = vec![
        EnhancedCandidate {
            text: "bar.txt".to_string(),
            description: None,
            candidate_type: CandidateType::File,
            priority: 0,
        },
        EnhancedCandidate {
            text: "bar".to_string(),
            description: None,
            candidate_type: CandidateType::Argument,
            priority: 0,
        },
    ];

    let boosted = engine.deduplicate_and_sort(candidates, 10, Some(&history_arc), None);
    let file_candidate = boosted.iter().find(|c| c.text == "bar.txt").unwrap();
    let arg_candidate = boosted.iter().find(|c| c.text == "bar").unwrap();

    assert_eq!(file_candidate.priority, 0);
    assert!(arg_candidate.priority > 0);
}

#[test]
fn history_boost_does_not_leak_to_same_text_file_candidates() {
    let mut history = crate::history::History::new();
    history.add_test_entry("git checkout shared");

    let candidates = vec![
        EnhancedCandidate {
            text: "shared".to_string(),
            description: None,
            candidate_type: CandidateType::File,
            priority: 0,
        },
        EnhancedCandidate {
            text: "shared".to_string(),
            description: None,
            candidate_type: CandidateType::Argument,
            priority: 0,
        },
    ];

    let scores = history_boost_scores(&candidates, &history, Some("git"));

    assert_eq!(scores[0], 0);
    assert!(scores[1] >= 500);
}

#[test]
fn test_context_aware_frecency_boost() {
    let environment = Environment::new();
    let engine = IntegratedCompletionEngine::new(environment);

    let mut history = crate::history::History::new();
    // Add history items: "git checkout" is frequent, "docker checkout" also exists
    history.add_test_entry("git checkout");
    history.add_test_entry("docker checkout");

    let history_arc = std::sync::Arc::new(parking_lot::Mutex::new(history));

    let create_candidate = || EnhancedCandidate {
        text: "checkout".to_string(),
        description: None,
        candidate_type: CandidateType::SubCommand,
        priority: 0,
    };

    // Case 1: Context is "git"
    let candidates_git = vec![create_candidate()];
    let boosted_git =
        engine.deduplicate_and_sort(candidates_git, 10, Some(&history_arc), Some("git"));
    let score_git = boosted_git[0].priority;

    // Case 2: Context is "npm" (irrelevant)
    let candidates_npm = vec![create_candidate()];
    let boosted_npm =
        engine.deduplicate_and_sort(candidates_npm, 10, Some(&history_arc), Some("npm"));
    let score_npm = boosted_npm[0].priority;

    // "git" context should boost "git checkout" history item highly
    // "npm" context should only get base boost
    assert!(
        score_git > score_npm,
        "Context match should produce higher priority. git: {}, npm: {}",
        score_git,
        score_npm
    );

    // Ensure the boost is substantial (our logic adds 500)
    assert!(score_git >= 500);
}

#[test]
fn test_context_aware_frecency_boost_edge_cases() {
    let environment = Environment::new();
    let engine = IntegratedCompletionEngine::new(environment);

    // Setup history
    // Setup history
    let mut history = crate::history::History::new();
    history.add_test_entry("git checkout");
    let history_arc = std::sync::Arc::new(parking_lot::Mutex::new(history));

    let create_candidate = || EnhancedCandidate {
        text: "checkout".to_string(), // Matches "git checkout"
        description: None,
        candidate_type: CandidateType::SubCommand,
        priority: 0,
    };

    // Case 1: No context (None)
    // Should only get base boost from matching text "checkout" inside "git checkout"
    let candidates_none = vec![create_candidate()];
    let boosted_none = engine.deduplicate_and_sort(candidates_none, 10, Some(&history_arc), None);
    let score_none = boosted_none[0].priority;

    // Case 2: Matching context ("git")
    // Should get high boost
    let candidates_git = vec![create_candidate()];
    let boosted_git =
        engine.deduplicate_and_sort(candidates_git, 10, Some(&history_arc), Some("git"));
    let score_git = boosted_git[0].priority;

    assert!(
        score_git > score_none,
        "Git context should boost higher than no context"
    );
    assert!(
        score_none > 0,
        "Even without context, text match should give some boost"
    );

    // Case 3: Mismatching context ("docker")
    // Should behave same as None/Low boost, or strictly less if logic changes?
    // Logic: if context is some, and doesn't match start, it skips the BIG boost.
    // But the item "git checkout" does NOT start with "docker".
    // So no BIG boost.
    // Base boost (+10) still applies because item contains "checkout".
    let candidates_docker = vec![create_candidate()];
    let boosted_docker =
        engine.deduplicate_and_sort(candidates_docker, 10, Some(&history_arc), Some("docker"));
    let score_docker = boosted_docker[0].priority;

    assert_eq!(
        score_docker, score_none,
        "Mismatch context should have same score as no context (base match)"
    );
}

#[tokio::test]
async fn git_checkout_completes_local_branches() {
    let dir = tempdir().unwrap();

    std::process::Command::new("git")
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    fs::write(dir.path().join("README.md"), "hello\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "README.md"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["checkout", "-b", "feature/test-branch"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "git checkout feat";
    let _ = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;

    let _ = wait_for_candidate(&engine, input, dir.path(), "feature/test-branch").await;
}

#[tokio::test]
async fn kubectl_context_option_value_uses_dynamic_provider() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let kubectl = bin_dir.join("kubectl");
    fs::write(
            &kubectl,
            "#!/bin/sh\nif [ \"$1\" = \"config\" ] && [ \"$2\" = \"get-contexts\" ]; then\n  printf 'dev-cluster\\nprod-cluster\\n'\nfi\n",
        )
        .unwrap();
    let mut permissions = fs::metadata(&kubectl).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&kubectl, permissions).unwrap();

    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![bin_dir.display().to_string()];
        env.clear_command_cache();
    }
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "kubectl --context de";
    let _ = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;

    let result = wait_for_candidate(&engine, input, dir.path(), "dev-cluster").await;
    assert_eq!(
        result.replacement_range,
        Some(CompletionReplacementRange { start: 18, end: 20 })
    );
}

#[tokio::test]
async fn kubectl_inline_context_option_value_uses_dynamic_provider() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let kubectl = bin_dir.join("kubectl");
    fs::write(
            &kubectl,
            "#!/bin/sh\nif [ \"$1\" = \"config\" ] && [ \"$2\" = \"get-contexts\" ]; then\n  printf 'dev-cluster\\nprod-cluster\\n'\nfi\n",
        )
        .unwrap();
    let mut permissions = fs::metadata(&kubectl).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&kubectl, permissions).unwrap();

    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![bin_dir.display().to_string()];
        env.clear_command_cache();
    }
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "kubectl --context=de";
    let _ = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;

    let result = wait_for_candidate(&engine, input, dir.path(), "dev-cluster").await;
    assert_eq!(
        result.replacement_range,
        Some(CompletionReplacementRange { start: 18, end: 20 })
    );

    let cached = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;

    assert!(
        cached
            .candidates
            .iter()
            .any(|candidate| candidate.text == "dev-cluster"),
        "expected cached kubectl context completion"
    );
    assert_eq!(
        cached.replacement_range,
        Some(CompletionReplacementRange { start: 18, end: 20 })
    );

    let cursor_inside_value = "kubectl --context=d".len();
    let middle = engine
        .complete(input, cursor_inside_value, dir.path(), 50, None)
        .await;
    assert_eq!(
        middle.replacement_range,
        Some(CompletionReplacementRange { start: 18, end: 20 })
    );
}

#[tokio::test]
async fn kubectl_short_attached_namespace_option_value_uses_dynamic_provider() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let kubectl = bin_dir.join("kubectl");
    fs::write(
            &kubectl,
            "#!/bin/sh\nif [ \"$1\" = \"get\" ] && [ \"$2\" = \"namespaces\" ]; then\n  printf 'dev-namespace\\nprod-namespace\\n'\nfi\n",
        )
        .unwrap();
    let mut permissions = fs::metadata(&kubectl).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&kubectl, permissions).unwrap();

    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![bin_dir.display().to_string()];
        env.clear_command_cache();
    }
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "kubectl -nde";
    let _ = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;

    let result = wait_for_candidate(&engine, input, dir.path(), "dev-namespace").await;
    assert_eq!(
        result.replacement_range,
        Some(CompletionReplacementRange { start: 10, end: 12 })
    );
}

#[test]
fn ghost_completion_uses_json_subcommands() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("checkout.txt"), "").unwrap();
    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "git che";
    let ghost = engine.ghost_completion(input, input.len(), dir.path(), None);

    assert_eq!(ghost.as_deref(), Some("git checkout"));
}

#[test]
fn ghost_completion_uses_json_subcommands_for_cargo() {
    let dir = tempdir().unwrap();
    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "cargo bu";
    let ghost = engine.ghost_completion(input, input.len(), dir.path(), None);

    assert_eq!(ghost.as_deref(), Some("cargo build"));
}

#[test]
fn ghost_completion_uses_json_choice_option_values() {
    let dir = tempdir().unwrap();
    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "ps --sort=me";
    let ghost = engine.ghost_completion(input, input.len(), dir.path(), None);

    assert_eq!(ghost.as_deref(), Some("ps --sort=mem"));
}

#[tokio::test]
async fn ghost_completion_uses_cached_kubectl_context() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let kubectl = bin_dir.join("kubectl");
    fs::write(
            &kubectl,
            "#!/bin/sh\nif [ \"$1\" = \"config\" ] && [ \"$2\" = \"get-contexts\" ]; then\n  printf 'dev-cluster\\nprod-cluster\\n'\nfi\n",
        )
        .unwrap();
    let mut permissions = fs::metadata(&kubectl).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&kubectl, permissions).unwrap();

    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![bin_dir.display().to_string()];
        env.clear_command_cache();
    }
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "kubectl --context=de";
    let _ = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    let _ = wait_for_candidate(&engine, input, dir.path(), "dev-cluster").await;

    let ghost = engine.ghost_completion(input, input.len(), dir.path(), None);
    assert_eq!(ghost.as_deref(), Some("kubectl --context=dev-cluster"));
}

#[tokio::test]
async fn ghost_completion_uses_cached_json_declared_dynamic_values_only() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let counter = dir.path().join("git-count");
    write_executable_script(
        &bin_dir.join("git"),
        &format!(
            "#!/bin/sh\ncount_file=\"{}\"\ncount=0\nif [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); fi\ncount=$((count + 1))\nprintf '%s' \"$count\" > \"$count_file\"\nif [ \"$1\" = \"stash\" ] && [ \"$2\" = \"list\" ]; then printf 'stash@{{0}}: WIP on main\\n'; fi\n",
            counter.display()
        ),
    );

    let engine = engine_with_path(&bin_dir);
    let input = "git stash pop stash";

    assert_eq!(
        engine.ghost_completion(input, input.len(), dir.path(), None),
        None
    );
    assert!(
        !counter.exists(),
        "ghost completion must not run uncached JSON-declared dynamic providers"
    );

    let _ = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    let _ = wait_for_candidate(&engine, input, dir.path(), "stash@{0}").await;

    assert_eq!(
        engine.ghost_completion(input, input.len(), dir.path(), None),
        Some("git stash pop stash@{0}".to_string())
    );
}

#[tokio::test]
async fn ghost_completion_uses_cached_new_local_dynamic_providers_only() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let counter = dir.path().join("busctl-count");
    write_executable_script(
        &bin_dir.join("busctl"),
        &format!(
            "#!/bin/sh\ncount_file=\"{}\"\ncount=0\nif [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); fi\ncount=$((count + 1))\nprintf '%s' \"$count\" > \"$count_file\"\nif [ \"$1\" = \"list\" ]; then printf 'org.freedesktop.login1 1 systemd root - - - Login\\n'; fi\n",
            counter.display()
        ),
    );

    let engine = engine_with_path(&bin_dir);
    let input = "busctl introspect org.";

    assert_eq!(
        engine.ghost_completion(input, input.len(), dir.path(), None),
        None
    );
    assert!(
        !counter.exists(),
        "ghost completion must not run uncached local dynamic providers"
    );

    let _ = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    let _ = wait_for_candidate(&engine, input, dir.path(), "org.freedesktop.login1").await;

    assert_eq!(
        engine.ghost_completion(input, input.len(), dir.path(), None),
        Some("busctl introspect org.freedesktop.login1".to_string())
    );
}

#[tokio::test]
async fn npm_run_completes_package_scripts() {
    let dir = tempdir().unwrap();
    let marker = dir.path().join("should-not-exist");
    fs::write(
        dir.path().join("package.json"),
        r#"{ "scripts": { "build": "vite build", "test": "vitest" } }"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("Makefile"),
        format!("$(shell touch {})\nall:\n\t@true\n", marker.display()),
    )
    .unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "npm run bu";
    let result = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "build"),
        "expected npm script completion in {:?}",
        result.candidates
    );
    assert!(
        !marker.exists(),
        "npm run completion must not invoke Makefile discovery"
    );
}

#[tokio::test]
async fn pnpm_run_completes_package_scripts() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("package.json"),
        r#"{ "scripts": { "bundle": "vite build", "test": "vitest" } }"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\n",
    )
    .unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "pnpm run bun";
    let result = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "bundle"),
        "expected pnpm script completion in {:?}",
        result.candidates
    );
}

#[tokio::test]
async fn npm_run_completes_package_scripts_even_when_lockfile_selects_another_manager() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("package.json"),
        r#"{ "scripts": { "build": "vite build", "test": "vitest" } }"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\n",
    )
    .unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "npm run bu";
    let result = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "build"),
        "expected npm script completion in {:?}",
        result.candidates
    );
}

#[tokio::test]
async fn yarn_completes_package_scripts_without_run_subcommand() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("package.json"),
        r#"{ "scripts": { "bundle": "vite build", "test": "vitest" } }"#,
    )
    .unwrap();
    fs::write(dir.path().join("yarn.lock"), "").unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "yarn bun";
    let result = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "bundle"),
        "expected yarn script completion in {:?}",
        result.candidates
    );
}

#[tokio::test]
async fn ghost_completion_uses_cached_json_declared_project_tasks() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("package.json"),
        r#"{ "scripts": { "build": "bun build ./src/index.ts", "test": "bun test" } }"#,
    )
    .unwrap();
    fs::write(dir.path().join("bun.lockb"), "").unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "bun run bu";
    assert_eq!(
        engine.ghost_completion(input, input.len(), dir.path(), None),
        None
    );

    let result = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "build"),
        "expected bun script completion in {:?}",
        result.candidates
    );

    assert_eq!(
        engine.ghost_completion(input, input.len(), dir.path(), None),
        Some("bun run build".to_string())
    );
}

#[tokio::test]
async fn deno_task_completes_deno_tasks() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("deno.json"),
        r#"{ "tasks": { "build": "deno run build.ts", "test": "deno test" } }"#,
    )
    .unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "deno task bu";
    let result = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "build"),
        "expected deno task completion in {:?}",
        result.candidates
    );
}

#[tokio::test]
async fn just_completes_project_recipes() {
    if std::process::Command::new("just")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }

    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("Justfile"),
        "test-recipe:\n\t@true\nbuild-recipe:\n\t@true\n",
    )
    .unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "just bu";
    let result = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "build-recipe"),
        "expected just recipe completion in {:?}",
        result.candidates
    );
}

#[tokio::test]
async fn make_completes_project_targets() {
    if std::process::Command::new("make")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }

    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("Makefile"),
        "test-target:\n\t@true\nbuild-target:\n\t@true\n",
    )
    .unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "make te";
    let result = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "test-target"),
        "expected make target completion in {:?}",
        result.candidates
    );
}

#[tokio::test]
async fn replacement_range_uses_entire_token_under_cursor() {
    let dir = tempdir().unwrap();
    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "pm li";
    let cursor_after_l = "pm l".len();
    let result = engine
        .complete(input, cursor_after_l, dir.path(), 50, None)
        .await;

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "list"),
        "expected pm subcommand completion"
    );
    assert_eq!(
        result.replacement_range,
        Some(CompletionReplacementRange { start: 3, end: 5 })
    );
}

#[tokio::test]
async fn external_completer_runs_as_fallback() {
    let dir = tempdir().unwrap();
    let environment = Environment::new();
    environment.write().variable_state.variables.insert(
            "DOGESH_EXTERNAL_COMPLETER".to_string(),
            "printf 'zzint-alpha\\tExternal completer\\n'; printf 'unrelated-candidate\\tExternal completer\\n'"
                .to_string(),
        );

    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "unknown-command zzint";
    let result = wait_for_candidate(&engine, input, dir.path(), "zzint-alpha").await;

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "zzint-alpha"),
        "expected external completer fallback"
    );
    assert_eq!(
        result.replacement_range,
        Some(CompletionReplacementRange { start: 16, end: 21 })
    );
}

#[tokio::test]
async fn fish_fallback_merges_below_json_candidates() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(
        &bin_dir.join("fish"),
        "#!/bin/sh\nprintf 'checkout\\tFish checkout\\nche-fish-only\\tFish only\\n'\n",
    );

    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![bin_dir.display().to_string()];
        env.clear_command_cache();
    }
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "git che";
    let started = std::time::Instant::now();
    let result = loop {
        let result = engine
            .complete(input, input.len(), dir.path(), 200, None)
            .await;
        if result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "che-fish-only")
        {
            break result;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "expected unique fish fallback candidate to be merged"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    let checkout = result
        .candidates
        .iter()
        .find(|candidate| candidate.text == "checkout")
        .expect("expected git checkout from JSON completion");
    assert_eq!(
        checkout.description.as_deref(),
        Some("Switch branches or restore working tree files")
    );
    let fish_candidate = result
        .candidates
        .iter()
        .find(|candidate| candidate.text == "che-fish-only")
        .unwrap();
    assert_eq!(fish_candidate.description.as_deref(), Some("Fish only"));
}

#[tokio::test]
async fn fish_fallback_still_runs_for_string_positional_arguments() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(
        &bin_dir.join("fish"),
        "#!/bin/sh\nprintf 'zzfish-package\\tFish package\\n'\n",
    );

    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![bin_dir.display().to_string()];
        env.clear_command_cache();
    }
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    let input = "apk add zzfish";
    let started = std::time::Instant::now();
    let result = loop {
        let result = engine
            .complete(input, input.len(), dir.path(), 200, None)
            .await;
        if result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "zzfish-package")
        {
            break result;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "expected fish fallback for String positional argument"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    let fish_candidate = result
        .candidates
        .iter()
        .find(|candidate| candidate.text == "zzfish-package")
        .unwrap();
    assert_eq!(fish_candidate.description.as_deref(), Some("Fish package"));
}

#[tokio::test]
async fn dynamic_providers_complete_script_backed_command_values() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::write(dir.path().join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    fs::write(
        dir.path().join("bacon.toml"),
        "[jobs.check]\ncommand = [\"cargo\", \"check\"]\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("pyproject.toml"),
        "[tool.pdm.scripts]\nlint = \"ruff check\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("Pipfile"),
        "[scripts]\nserve = \"python -m app\"\n",
    )
    .unwrap();
    fs::write(dir.path().join("meson.build"), "project('demo', 'c')\n").unwrap();
    fs::create_dir_all(dir.path().join("build")).unwrap();
    fs::create_dir_all(dir.path().join(".jj")).unwrap();

    write_executable_script(
        &bin_dir.join("cargo"),
        r#"#!/bin/sh
if [ "$1" = "metadata" ]; then
cat <<'JSON'
{"packages":[{"name":"app-core","targets":[{"name":"cli-tool","kind":["bin"]},{"name":"demo-example","kind":["example"]}]}]}
JSON
fi
"#,
    );
    write_executable_script(
        &bin_dir.join("git"),
        r#"#!/bin/sh
args="$*"
if [ "$1" = "for-each-ref" ]; then
  case "$args" in
    *"refs/heads"*"refs/remotes"*) printf 'main\norigin/release\norigin/HEAD\n' ;;
    *"refs/remotes"*) printf 'origin/main\norigin/release\nupstream/dev\norigin/HEAD\n' ;;
    *"refs/heads"*"refs/tags"*) printf 'main\nv1.0.0\n' ;;
    *"refs/heads"*) printf 'main\nfeature/demo\n' ;;
  esac
elif [ "$1" = "remote" ]; then
  printf 'origin\nupstream\n'
elif [ "$1" = "tag" ]; then
  printf 'v1.0.0\n'
elif [ "$1" = "stash" ] && [ "$2" = "list" ]; then
  printf 'stash@{0}: WIP on main\nstash@{1}: On dev\n'
elif [ "$1" = "status" ]; then
  printf ' M src/lib.rs\0?? README.md\0'
elif [ "$1" = "worktree" ] && [ "$2" = "list" ]; then
  printf 'worktree /tmp/demo-worktree\nHEAD abc123\n'
fi
"#,
    );
    write_executable_script(
        &bin_dir.join("systemctl"),
        "#!/bin/sh\ncase \"$1\" in\nlist-unit-files) printf 'ssh.service enabled\\n';;\nlist-units) printf 'docker.service loaded active running Docker\\n';;\nesac\n",
    );
    write_executable_script(
        &bin_dir.join("tmux"),
        "#!/bin/sh\nif [ \"$1\" = \"list-sessions\" ]; then printf 'dev-session\\nprod-session\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("gh"),
        "#!/bin/sh\nif [ \"$1\" = \"pr\" ] && [ \"$2\" = \"list\" ]; then printf '123\\n124\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("pip"),
        "#!/bin/sh\nif [ \"$1\" = \"list\" ]; then printf 'requests==2.0.0\\npytest==8.0.0\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("rustup"),
        "#!/bin/sh\nif [ \"$1\" = \"toolchain\" ] && [ \"$2\" = \"list\" ]; then printf 'stable-x86_64-unknown-linux-gnu (default)\\nnightly-x86_64-unknown-linux-gnu\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("nmcli"),
        "#!/bin/sh\nif [ \"$4\" = \"connection\" ]; then printf 'home-wifi\\nwork-vpn\\n'; elif [ \"$4\" = \"device\" ] && [ \"$5\" = \"status\" ]; then printf 'wlan0:connected\\neth0:disconnected\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("pacman"),
        // `visual-studio-code-bin` stands in for an AUR package: it is
        // installed (`-Qq`) but absent from the sync repositories (`-Slq`),
        // so a removal completion sourced from the wrong list loses it.
        "#!/bin/sh\nfor a in \"$@\"; do\n  case \"$a\" in\n    -Qq) printf 'pacman\\nparu\\nvisual-studio-code-bin\\n'; exit 0 ;;\n    -Slq) printf 'ripgrep\\nrust\\n'; exit 0 ;;\n  esac\ndone\nexit 0\n",
    );
    write_executable_script(
        &bin_dir.join("snapper"),
        "#!/bin/sh\nprintf '{\"snapshots\":[{\"number\":1},{\"number\":42}]}'\n",
    );
    write_executable_script(
        &bin_dir.join("jj"),
        "#!/bin/sh\nif [ \"$1\" = \"--repository\" ]; then shift 2; fi\ncase \"$1 $2\" in\n'bookmark list') printf 'main\\nrelease\\n';;\n'workspace list') printf 'default\\ndocs\\n';;\n*) printf 'abc123\\ndef456\\n';;\nesac\n",
    );
    write_executable_script(
        &bin_dir.join("meson"),
        "#!/bin/sh\nprintf '[{\"name\":\"app\"},{\"name\":\"tests\"}]'\n",
    );
    write_executable_script(
        &bin_dir.join("ghq"),
        "#!/bin/sh\nif [ \"$1\" = \"list\" ]; then printf 'github.com/org/repo\\ngitlab.com/org/tools\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("golangci-lint"),
        "#!/bin/sh\nif [ \"$1\" = \"linters\" ]; then printf 'errcheck: check errors\\ngovet: vet code\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("docker"),
        "#!/bin/sh\nif [ \"$1\" = \"ps\" ]; then printf 'app-container\\nworker-container\\n'; elif [ \"$1\" = \"images\" ]; then printf 'app-image:latest\\nbase-image:latest\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("kubectl"),
        "#!/bin/sh\nif [ \"$1\" = \"get\" ] && [ \"$2\" = \"pods\" ]; then printf 'web-0\\napi-0\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("lsblk"),
        "#!/bin/sh\nif [ \"$1\" = \"-rno\" ]; then printf 'sda disk\\nsda1 part\\nloop0 loop\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("blkid"),
        "#!/bin/sh\nif [ \"$1\" = \"-o\" ]; then printf 'DEVNAME=/dev/sda1\\nUUID=abcd-1234\\nLABEL=rootfs\\n\\nDEVNAME=/dev/sdb1\\nUUID=beef-9999\\nLABEL=data\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("busctl"),
        "#!/bin/sh\nif [ \"$1\" = \"list\" ]; then printf 'org.freedesktop.login1 1 systemd root - - - Login\\norg.example.Demo 2 demo user - - - Demo\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("dpkg-query"),
        "#!/bin/sh\nif [ \"$1\" = \"-W\" ]; then printf 'base-files\\nbash\\ncoreutils\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("localectl"),
        "#!/bin/sh\ncase \"$1\" in\nlist-keymaps) printf 'jp106\\nus\\n';;\nlist-locales) printf 'en_US.UTF-8\\nja_JP.UTF-8\\n';;\nesac\n",
    );
    write_executable_script(
        &bin_dir.join("loginctl"),
        "#!/bin/sh\ncase \"$1\" in\nlist-sessions) printf '2 1000 alice seat0 tty2\\n';;\nlist-seats) printf 'seat0\\n';;\nesac\n",
    );
    write_executable_script(
        &bin_dir.join("losetup"),
        "#!/bin/sh\nif [ \"$1\" = \"--list\" ]; then printf '/dev/loop0\\n/dev/loop1\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("rpm"),
        "#!/bin/sh\nif [ \"$1\" = \"-qa\" ]; then printf 'kernel-core\\nbash\\nsystemd\\n'; fi\n",
    );
    write_executable_script(
        &bin_dir.join("timedatectl"),
        "#!/bin/sh\nif [ \"$1\" = \"list-timezones\" ]; then printf 'Asia/Tokyo\\nEurope/London\\n'; fi\n",
    );

    let engine = engine_with_path(&bin_dir);

    let cases = [
        ("cargo build -p ap", "app-core"),
        ("cargo run --bin cl", "cli-tool"),
        ("git switch fe", "feature/demo"),
        ("git checkout rel", "release"),
        ("git push origin fe", "feature/demo"),
        ("git pull origin ma", "main"),
        ("git tag v1", "v1.0.0"),
        ("git stash pop stash", "stash@{0}"),
        ("git add RE", "README.md"),
        ("git worktree remove demo", "/tmp/demo-worktree"),
        ("systemctl start ss", "ssh.service"),
        ("journalctl -u do", "docker.service"),
        ("tmux attach -t de", "dev-session"),
        ("gh pr view 12", "123"),
        ("pip show req", "requests"),
        ("rustup default sta", "stable-x86_64-unknown-linux-gnu"),
        ("nmcli connection up ho", "home-wifi"),
        ("nmcli device disconnect wl", "wlan0"),
        ("pacman -R pa", "pacman"),
        ("yay -S ri", "ripgrep"),
        ("paru -R pa", "pacman"),
        // AUR packages only exist in the local database, so removal must
        // read `-Qq`.
        ("pacman -R visual", "visual-studio-code-bin"),
        // Bundled operation flags (`-Rns` is the usual removal spelling).
        ("pacman -Rns visual", "visual-studio-code-bin"),
        ("pacman -Syu ri", "ripgrep"),
        ("yay -Rns visual", "visual-studio-code-bin"),
        // Wrapped in `sudo`, which is how removal is actually typed.
        ("sudo pacman -R visual", "visual-studio-code-bin"),
        ("sudo pacman -Rns visual", "visual-studio-code-bin"),
        ("sudo -u root pacman -R pa", "pacman"),
        // Unwrapping is generic over `CommandWithArgs`, not sudo-specific.
        ("sudo systemctl start ss", "ssh.service"),
        ("sudo docker stop app", "app-container"),
        ("snapper --config root delete 4", "42"),
        ("snapper --config root status 1..4", "1..42"),
        ("snapper --config root delete 1-4", "1-42"),
        ("jj bookmark delete ma", "main"),
        ("jj rebase --destination ab", "abc123"),
        ("jj workspace forget de", "default"),
        ("bacon ch", "check"),
        ("pdm run li", "lint"),
        ("pipenv run se", "serve"),
        ("meson compile -C build ap", "app"),
        ("ghq look github", "github.com/org/repo"),
        ("golangci-lint run --enable err", "errcheck"),
        ("cargo nextest run -p ap", "app-core"),
        ("docker stop app", "app-container"),
        ("docker inspect app-i", "app-image:latest"),
        ("kubectl get pods we", "web-0"),
        ("fdisk /dev/s", "/dev/sda"),
        ("mount /dev/lo", "/dev/loop0"),
        ("blkid -U ab", "abcd-1234"),
        ("blkid -L root", "rootfs"),
        ("busctl introspect org.", "org.freedesktop.login1"),
        ("localectl set-keymap jp", "jp106"),
        ("localectl set-locale en", "en_US.UTF-8"),
        ("loginctl session-status 2", "2"),
        ("loginctl seat-status seat", "seat0"),
        ("losetup -d /dev/loop", "/dev/loop0"),
        ("timedatectl set-timezone Asia/T", "Asia/Tokyo"),
        ("apt remove bas", "base-files"),
        ("dnf remove ker", "kernel-core"),
        ("yum remove sys", "systemd"),
    ];

    for (input, expected) in cases {
        let _ = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;
        let _ = wait_for_candidate(&engine, input, dir.path(), expected).await;
    }
}

/// AUR packages are installed but absent from the sync repositories, so
/// they only ever appear if removal reads the local database. Sourcing the
/// sync list instead is exactly the bug this guards: it still looks
/// plausible (every repo-installed package is there) while silently
/// dropping everything from the AUR.
#[test]
fn dynamic_provider_target_unwraps_command_wrappers() {
    let mut engine = IntegratedCompletionEngine::new(Environment::new());
    engine.initialize_command_completion().unwrap();

    let cases = [
        ("sudo pacman -R ", "pacman"),
        ("sudo pacman -R pa", "pacman"),
        ("sudo -u root pacman -R pa", "pacman"),
        ("sudo systemctl start ss", "systemctl"),
    ];
    for (input, expected_command) in cases {
        let parsed = engine.convert_to_parsed_command_line(input, input.len());
        let target = engine.dynamic_provider_target(&parsed);
        assert_eq!(
            target.command, expected_command,
            "unexpected unwrap target for {input}"
        );
    }

    // Typing the wrapped command's own name still completes command names.
    let parsed = engine.convert_to_parsed_command_line("sudo pac", "sudo pac".len());
    let target = engine.dynamic_provider_target(&parsed);
    assert_eq!(target.command, "sudo");

    // Unwrapped lines are normalized like a line typed on its own, so the
    // declared provider can resolve the `-R` argument definition.
    let parsed = engine.convert_to_parsed_command_line("sudo pacman -R ", "sudo pacman -R ".len());
    let target = engine.dynamic_provider_target(&parsed);
    assert_eq!(target.subcommand_path, vec!["-R".to_string()]);

    // Not a wrapper: borrowed through untouched.
    let parsed = engine.convert_to_parsed_command_line("pacman -R ", "pacman -R ".len());
    let target = engine.dynamic_provider_target(&parsed);
    assert!(matches!(target, Cow::Borrowed(_)));
}

#[tokio::test]
async fn pacman_removal_offers_aur_packages_and_install_does_not() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(
        &bin_dir.join("pacman"),
        "#!/bin/sh\nfor a in \"$@\"; do\n  case \"$a\" in\n    -Qq) printf 'pacman\\nvisual-studio-code-bin\\n'; exit 0 ;;\n    -Slq) printf 'ripgrep\\nvisual-studio-code\\n'; exit 0 ;;\n  esac\ndone\nexit 0\n",
    );

    let engine = engine_with_path(&bin_dir);

    for input in [
        "pacman -R visual",
        "pacman -Rns visual",
        "sudo pacman -R visual",
        "sudo pacman -Rns visual",
    ] {
        let _ = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;
        let _ = wait_for_candidate(&engine, input, dir.path(), "visual-studio-code-bin").await;
    }

    // Installing reads the sync list, so the AUR package must not show up.
    let input = "pacman -S visual";
    let _ = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    let result = wait_for_candidate(&engine, input, dir.path(), "visual-studio-code").await;
    assert!(
        !result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "visual-studio-code-bin"),
        "pacman -S offered a package that is only in the local database: {:?}",
        result.candidates
    );
}

#[tokio::test]
async fn jj_dynamic_completion_uses_selected_repository() {
    let dir = tempdir().unwrap();
    let current_repo = dir.path().join("current");
    let selected_repo = dir.path().join("selected");
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(current_repo.join(".jj")).unwrap();
    fs::create_dir_all(selected_repo.join(".jj")).unwrap();
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(
        &bin_dir.join("jj"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" != \"--repository\" ]; then exit 9; fi\nif [ \"$2\" = \"{}\" ] && [ \"$3 $4\" = \"bookmark list\" ]; then printf 'target-main\\n'; else printf 'wrong-repository\\n'; fi\n",
            selected_repo.display()
        ),
    );

    let engine = engine_with_path(&bin_dir);
    let input = format!("jj -R {} bookmark delete target", selected_repo.display());
    let result = wait_for_candidate(&engine, &input, &current_repo, "target-main").await;
    assert!(
        !result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "wrong-repository"),
        "jj provider used the current repository instead of -R: {:?}",
        result.candidates
    );
}

/// A prefix and the key it must reach, for whichever kernel is running.
///
/// The two `load_sysctl_keys` branches read different sources -- a walk of
/// `/proc/sys` on Linux, `sysctl -aN` on macOS -- so a shared key would
/// leave one of them untested. This used to be Linux's key alone behind a
/// bare existence check, which made the whole test pass silently on macOS
/// and left the `sysctl -aN` path unexercised.
#[cfg(not(target_os = "macos"))]
fn sysctl_probe() -> Option<(&'static str, &'static str)> {
    // A container can be built without the ipv4 sysctl tree, and nothing
    // else under /proc/sys is both universal and nested deeply enough to
    // exercise the dotted-prefix path.
    Path::new("/proc/sys/net/ipv4/ip_forward")
        .exists()
        .then_some(("net.ipv4.ip_for", "net.ipv4.ip_forward"))
}

/// See the Linux probe above. `kern.ostype` is in every macOS kernel, so
/// there is nothing to skip on.
#[cfg(target_os = "macos")]
fn sysctl_probe() -> Option<(&'static str, &'static str)> {
    Some(("kern.ostyp", "kern.ostype"))
}

#[tokio::test]
async fn sysctl_key_completion_uses_this_platforms_keys_without_value_side() {
    let Some((prefix, key)) = sysctl_probe() else {
        return;
    };

    let dir = tempdir().unwrap();
    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    // `sysctl.key` goes through the cached-value path, whose first call
    // always returns empty and only schedules the background refresh, so
    // the candidate has to be waited for rather than asserted on directly.
    let input = format!("sysctl {prefix}");
    wait_for_candidate(&engine, &input, dir.path(), key).await;

    let value_input = format!("sysctl {key}=1");
    let value_result = engine
        .complete(&value_input, value_input.len(), dir.path(), 50, None)
        .await;
    assert!(
        !value_result
            .candidates
            .iter()
            .any(|candidate| candidate.text == key),
        "sysctl key provider must not complete the value side"
    );
}

#[tokio::test]
async fn js_dependency_completion_uses_package_json_without_script() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("package.json"),
        r#"{"dependencies":{"react":"latest"},"devDependencies":{"vite":"latest"}}"#,
    )
    .unwrap();

    let mut engine = IntegratedCompletionEngine::new(Environment::new());
    engine.initialize_command_completion().unwrap();

    let input = "npm uninstall rea";
    let result = wait_for_candidate(&engine, input, dir.path(), "react").await;

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "react"),
        "expected package.json dependency completion in {:?}",
        result.candidates
    );
}

#[tokio::test]
async fn ghost_completion_uses_cached_dynamic_values_only() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let counter = dir.path().join("tmux-count");
    write_executable_script(
        &bin_dir.join("tmux"),
        &format!(
            "#!/bin/sh\ncount_file=\"{}\"\ncount=0\nif [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); fi\ncount=$((count + 1))\nprintf '%s' \"$count\" > \"$count_file\"\nif [ \"$1\" = \"list-sessions\" ]; then printf 'dev-session\\n'; fi\n",
            counter.display()
        ),
    );

    let engine = engine_with_path(&bin_dir);
    let input = "tmux attach -t de";

    assert_eq!(
        engine.ghost_completion(input, input.len(), dir.path(), None),
        None
    );
    assert!(
        !counter.exists(),
        "ghost completion must not run uncached dynamic commands"
    );

    let _ = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    let _ = wait_for_candidate(&engine, input, dir.path(), "dev-session").await;

    assert_eq!(
        engine.ghost_completion(input, input.len(), dir.path(), None),
        Some("tmux attach -t dev-session".to_string())
    );
}

#[tokio::test]
async fn dev_dynamic_providers_complete_local_project_values() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("go.mod"), "module example.com/demo\n").unwrap();
    fs::write(
        dir.path().join("pyproject.toml"),
        "[project]\ndependencies = [\"requests>=2\", \"pytest\"]\n",
    )
    .unwrap();
    fs::write(dir.path().join("requirements-dev.txt"), "ruff==0.8\n").unwrap();
    let nested_go_dir = dir.path().join("pkg");
    fs::create_dir_all(&nested_go_dir).unwrap();

    let node_bin = dir.path().join("node_modules").join(".bin");
    fs::create_dir_all(&node_bin).unwrap();
    fs::write(node_bin.join("vite"), "").unwrap();
    fs::write(node_bin.join("eslint"), "").unwrap();

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(
        &bin_dir.join("go"),
        "#!/bin/sh\nif [ \"$1\" = list ]; then printf 'example.com/demo\\t%s\\n' \"$PWD\"; printf 'example.com/demo/pkg/api\\t%s/pkg/api\\n' \"$PWD\"; fi\n",
    );

    let engine = engine_with_path(&bin_dir);
    let cases = [
        ("uv remove req", "requests"),
        ("poetry remove py", "pytest"),
        ("npx vi", "vite"),
        ("npm exec es", "eslint"),
        ("go test ./p", "./pkg/api"),
    ];

    for (input, expected) in cases {
        let _ = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;
        let _ = wait_for_candidate(&engine, input, dir.path(), expected).await;
    }

    let input = "go test ./p";
    let _ = engine
        .complete(input, input.len(), &nested_go_dir, 50, None)
        .await;
    let _ = wait_for_candidate(&engine, input, &nested_go_dir, "./pkg/api").await;
}

#[tokio::test]
async fn python_module_dynamic_providers_complete_local_modules() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("pyproject.toml"),
        "[project]\ndependencies = [\"fastapi>=0.110\"]\n",
    )
    .unwrap();
    let package_dir = dir.path().join("src").join("demo_app");
    fs::create_dir_all(&package_dir).unwrap();
    fs::write(package_dir.join("__init__.py"), "").unwrap();
    fs::write(package_dir.join("cli.py"), "").unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    for (input, expected) in [
        ("python -m dem", "demo_app"),
        ("python3 -m demo_app.c", "demo_app.cli"),
        ("pytest --cov dem", "demo_app"),
        ("mypy -m demo_app.c", "demo_app.cli"),
        ("mypy -p fast", "fastapi"),
    ] {
        let result = wait_for_candidate(&engine, input, dir.path(), expected).await;
        assert!(
            result
                .candidates
                .iter()
                .all(|candidate| candidate.candidate_type == CandidateType::Argument),
            "{input} should return module argument candidates: {:?}",
            result.candidates
        );
    }
}

#[tokio::test]
async fn node_workspace_and_bin_completion_walks_monorepo_ancestors() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("package.json"),
        r#"{ "private": true, "workspaces": ["packages/*"] }"#,
    )
    .unwrap();
    let package_dir = dir.path().join("packages").join("web");
    fs::create_dir_all(&package_dir).unwrap();
    fs::write(
        package_dir.join("package.json"),
        r#"{ "name": "@demo/web", "scripts": { "build": "vite build" } }"#,
    )
    .unwrap();
    let node_bin = dir.path().join("node_modules").join(".bin");
    fs::create_dir_all(&node_bin).unwrap();
    fs::write(node_bin.join("vite"), "").unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    for (input, expected) in [
        ("npx vi", "vite"),
        ("npm exec vi", "vite"),
        ("pnpm exec vi", "vite"),
        ("yarn exec vi", "vite"),
        ("bun x vi", "vite"),
        ("npm --workspace @demo", "@demo/web"),
        ("pnpm --filter @demo", "@demo/web"),
        ("yarn workspace @demo", "@demo/web"),
        ("turbo run build --filter @demo", "@demo/web"),
    ] {
        let _ = engine
            .complete(input, input.len(), &package_dir, 50, None)
            .await;
        let _ = wait_for_candidate(&engine, input, &package_dir, expected).await;
    }
}

#[tokio::test]
async fn cloud_and_terraform_dynamic_providers_read_local_fixtures() {
    let dir = tempdir().unwrap();
    let aws_dir = dir.path().join(".aws");
    fs::create_dir_all(&aws_dir).unwrap();
    let aws_config = aws_dir.join("config");
    let aws_credentials = aws_dir.join("credentials");
    fs::write(
        &aws_config,
        "[default]\nregion = us-east-1\n[profile dev]\nregion = us-west-2\n",
    )
    .unwrap();
    fs::write(&aws_credentials, "[prod]\naws_access_key_id = test\n").unwrap();

    let gcloud_dir = dir.path().join("gcloud");
    let gcloud_configurations = gcloud_dir.join("configurations");
    fs::create_dir_all(&gcloud_configurations).unwrap();
    fs::write(
        gcloud_configurations.join("config_dev"),
        "project = demo-dev\n",
    )
    .unwrap();
    fs::write(
        gcloud_configurations.join("config_prod"),
        "project = demo-prod\n",
    )
    .unwrap();

    let terraform_dir = dir.path().join(".terraform");
    fs::create_dir_all(terraform_dir.join("terraform.tfstate.d").join("dev")).unwrap();
    fs::write(terraform_dir.join("environment"), "staging\n").unwrap();

    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.set_and_export_shell_var("HOME".to_string(), dir.path().display().to_string());
        env.set_and_export_shell_var(
            "AWS_CONFIG_FILE".to_string(),
            aws_config.display().to_string(),
        );
        env.set_and_export_shell_var(
            "AWS_SHARED_CREDENTIALS_FILE".to_string(),
            aws_credentials.display().to_string(),
        );
        env.set_and_export_shell_var(
            "CLOUDSDK_CONFIG".to_string(),
            gcloud_dir.display().to_string(),
        );
    }
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    for (input, expected) in [
        ("aws --profile de", "dev"),
        ("gcloud --configuration de", "dev"),
        ("gcloud --project demo-p", "demo-prod"),
        ("terraform workspace select sta", "staging"),
        ("tofu workspace delete de", "dev"),
    ] {
        let result = wait_for_candidate(&engine, input, dir.path(), expected).await;
        assert!(
            result
                .candidates
                .iter()
                .all(|candidate| candidate.candidate_type == CandidateType::Argument),
            "{input} should return local config argument candidates: {:?}",
            result.candidates
        );
    }
}

#[tokio::test]
async fn container_dynamic_providers_complete_fake_cli_objects() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let script = r#"#!/bin/sh
case "$1" in
  images) printf 'localhost/app:latest\n<none>:<none>\n' ;;
  ps)
    if [ "$2" = "-a" ]; then
      printf 'web\nold\n'
    else
      printf 'web\n'
    fi
    ;;
  network)
    if [ "$2" = "ls" ]; then printf 'frontend\nbackend\n'; fi
    ;;
  volume)
    if [ "$2" = "ls" ]; then printf 'cache\nlogs\n'; fi
    ;;
esac
"#;
    write_executable_script(&bin_dir.join("docker"), script);
    write_executable_script(&bin_dir.join("podman"), script);

    let engine = engine_with_path(&bin_dir);
    for (input, expected) in [
        ("docker run loc", "localhost/app:latest"),
        ("docker rm ol", "old"),
        ("docker network rm fr", "frontend"),
        ("docker volume rm ca", "cache"),
        ("podman run loc", "localhost/app:latest"),
        ("podman stop we", "web"),
        ("podman network inspect back", "backend"),
        ("podman volume inspect lo", "logs"),
    ] {
        let _ = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;
        let _ = wait_for_candidate(&engine, input, dir.path(), expected).await;
    }
}

#[tokio::test]
async fn nx_run_completion_reads_workspace_and_descendant_projects() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("workspace.json"),
        r#"{
              "projects": {
                "web": { "targets": { "build": {}, "test": {} } },
                "legacy": { "architect": { "serve": {} } }
              }
            }"#,
    )
    .unwrap();
    let api_dir = dir.path().join("apps").join("api");
    fs::create_dir_all(&api_dir).unwrap();
    fs::write(
        api_dir.join("project.json"),
        r#"{ "name": "api", "targets": { "lint": {} } }"#,
    )
    .unwrap();
    let tasks = dsh_builtin::task::list_tasks_in_dir_for_sources(
        dir.path(),
        &["nx"],
        &dsh_builtin::task::TaskDiscoveryRuntime::new(Vec::new(), std::collections::HashMap::new()),
    )
    .unwrap();
    assert!(
        tasks.iter().any(|task| task.command == "nx run web:build"),
        "expected Nx task detection to include web:build: {tasks:?}"
    );

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    for (input, expected) in [
        ("nx run web:b", "web:build"),
        ("nx run legacy:s", "legacy:serve"),
        ("nx run api:l", "api:lint"),
    ] {
        let result = wait_for_candidate(&engine, input, dir.path(), expected).await;
        assert!(
            !result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "build"),
            "nx run should use project:target candidates, not bare targets: {:?}",
            result.candidates
        );
    }
}

#[tokio::test]
async fn project_task_scope_limits_dev_task_sources() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("package.json"),
        r#"{ "scripts": { "build-npm": "echo npm" } }"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("deno.json"),
        r#"{ "tasks": { "build-deno": "deno run main.ts" } }"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("turbo.json"),
        r#"{ "tasks": { "build-turbo": {} } }"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        r#"{ "name": "app", "targets": { "build-nx": {} } }"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("mise.toml"),
        "[tasks.build-mise]\nrun = 'echo mise'\n",
    )
    .unwrap();

    let environment = Environment::new();
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();

    for (input, expected) in [
        ("deno task bu", "build-deno"),
        ("turbo run bu", "build-turbo"),
        ("nx run app:b", "app:build-nx"),
        ("mise run bu", "build-mise"),
    ] {
        let result = wait_for_candidate(&engine, input, dir.path(), expected).await;
        assert!(
            !result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "build-npm"),
            "{input} should not include npm tasks from another project.task scope: {:?}",
            result.candidates
        );
    }
}
