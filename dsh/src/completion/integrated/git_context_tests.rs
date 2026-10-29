use super::*;
use std::fs;
use tokio::sync::mpsc::UnboundedReceiver;

fn engine() -> IntegratedCompletionEngine {
    let env = Environment::new_isolated();
    env.write()
        .set_shell_var("DOGESH_EXTERNAL_COMPLETER".into(), String::new());
    env.write()
        .set_shell_var("DOGESH_COMPLETION_FISH_FALLBACK".into(), "off".into());
    let mut engine = IntegratedCompletionEngine::new(env);
    engine.initialize_command_completion().unwrap();
    engine
}

#[test]
fn git_global_option_values_preserve_subcommand_and_option_context() {
    let engine = engine();
    for (input, subcommand) in [
        ("git -c color.ui=false commit --a", "commit"),
        ("git -C /somewhere switch topic", "switch"),
    ] {
        let parsed = engine.convert_to_parsed_command_line(input, input.len());
        assert_eq!(parsed.subcommand_path, vec![subcommand], "{input}");
    }
    for input in ["git -C ", "git -C 2>/dev/null re"] {
        let parsed = engine.convert_to_parsed_command_line(input, input.len());
        assert!(
            matches!(
                engine.argument_type_for_completion_context(&parsed),
                Some(ArgumentType::Directory)
            ),
            "{input}: {parsed:?}"
        );
    }
}

#[tokio::test]
async fn embedded_repeated_options_remain_available() {
    let engine = engine();
    let dir = tempfile::tempdir().unwrap();
    for (input, expected) in [
        ("grep -e foo -", "-e"),
        ("grep --regexp foo --", "--regexp"),
        ("grep -f patterns -", "-f"),
        ("git -C /tmp -", "-C"),
        ("git -c color.ui=false -", "-c"),
        ("git commit -m first -", "-m"),
        ("code --install-extension first --", "--install-extension"),
    ] {
        let result = engine
            .complete(input, input.len(), dir.path(), 100, None)
            .await;
        assert!(
            result.candidates.iter().any(|c| c.text == expected),
            "{input}: {:?}",
            result.candidates
        );
    }
}

#[tokio::test]
async fn option_synonyms_do_not_hide_different_subcommand_options() {
    let engine = engine();
    let dir = tempfile::tempdir().unwrap();
    for (input, expected) in [
        ("git -p fetch --pr", "--prune"),
        ("git -p commit --pa", "--patch"),
    ] {
        let result = engine
            .complete(input, input.len(), dir.path(), 100, None)
            .await;
        assert!(
            result.candidates.iter().any(|c| c.text == expected),
            "{input}: {:?}",
            result.candidates
        );
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Completion Test",
            "-c",
            "user.email=completion@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

async fn settle(
    engine: &IntegratedCompletionEngine,
    rx: &mut UnboundedReceiver<()>,
    input: &str,
    cwd: &Path,
    expected: &str,
) -> CompletionResult {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let result = engine.complete(input, input.len(), cwd, 100, None).await;
        if result.candidates.iter().any(|c| c.text == expected) {
            return result;
        }
        tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("{input}: expected {expected}, got {:?}", result.candidates))
            .unwrap();
    }
}

#[tokio::test]
async fn git_directory_options_scope_dynamic_paths_and_cached_ghosts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for (name, branch) in [("repo-a", "only_a"), ("repo b", "only_b")] {
        let repo = root.join(name);
        fs::create_dir(&repo).unwrap();
        fs::create_dir(repo.join("nested")).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["commit", "--allow-empty", "-qm", "fixture"]);
        git(&repo, &["branch", branch]);
        fs::write(repo.join(format!("file_{branch}")), "").unwrap();
        fs::write(repo.join("-r"), "").unwrap();
    }
    let engine = engine();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    engine.set_notifier(tx);
    engine
        .environment
        .write()
        .set_shell_var("HOME".into(), root.display().to_string());
    for (prefix, expected) in [
        ("git -C repo-a", "only_a"),
        ("git 2>/dev/null -C repo-a", "only_a"),
        ("git -C 2>/dev/null repo-a", "only_a"),
        ("git -c 2>/dev/null color.ui=false -C repo-a", "only_a"),
        ("git -C 'repo b'", "only_b"),
        ("git -C 'repo b' -C nested", "only_b"),
        ("git -C '' -C'repo b'", "only_b"),
        ("git -c color.ui=false -C'repo b'", "only_b"),
        (r"git -C ~/repo\ b", "only_b"),
        ("sudo git -C 'repo b'", "only_b"),
        ("git -C repo-a", "only_a"),
    ] {
        let input = format!("{prefix} switch only_");
        let result = settle(&engine, &mut rx, &input, &root, expected).await;
        assert_eq!(
            result
                .candidates
                .iter()
                .filter(|c| c.text.starts_with("only_"))
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            vec![expected]
        );
        assert_eq!(
            engine.ghost_completion(&input, input.len(), &root, None),
            Some(format!("{prefix} switch {expected}"))
        );
    }
    let input = "git -C 'repo b' checkout file_";
    let result = engine.complete(input, input.len(), &root, 100, None).await;
    assert!(result.candidates.iter().any(|c| c.text == "file_only_b"));
    let input = "git -C 'repo b' -C nes";
    let result = engine.complete(input, input.len(), &root, 100, None).await;
    assert!(
        result
            .candidates
            .iter()
            .any(|c| c.text.trim_end_matches('/') == "nested"
                && c.candidate_type == CandidateType::Directory),
        "{:?}",
        result.candidates
    );
    let input = "git -C repo-a switch only_ -C 'repo b'";
    let cursor = "git -C repo-a switch only_".len();
    let result = engine.complete(input, cursor, &root, 100, None).await;
    assert!(result.candidates.iter().any(|c| c.text == "only_a"));
    assert!(!result.candidates.iter().any(|c| c.text == "only_b"));
    let input = "git -C 'repo b' -C nested add ";
    let result = settle(&engine, &mut rx, input, &root, "../file_only_b").await;
    assert!(result.candidates.iter().all(|c| !c.text.contains("only_a")));
    let input = "git -C 'repo b' add ";
    let result = settle(&engine, &mut rx, input, &root, "-r").await;
    let candidate = result
        .candidates
        .into_iter()
        .find(|c| c.text == "-r")
        .unwrap();
    use skim::SkimItem;
    let formatted = crate::completion::shell_path::format_candidates_for_token(
        engine.to_candidates(vec![candidate]),
        None,
    );
    assert_eq!(formatted[0].output(), "./-r");
}

#[tokio::test]
async fn external_git_completion_receives_original_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::create_dir(root.join("nested")).unwrap();
    let engine = engine();
    engine.environment.write().set_shell_var(
        "DOGESH_EXTERNAL_COMPLETER".into(),
        "printf 'zz-context\\t%s\\n' \"$PWD\"".into(),
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    engine.set_notifier(tx);
    let input = "git -C nested unknown zz";
    let result = settle(&engine, &mut rx, input, &root, "zz-context").await;
    let candidate = result
        .candidates
        .iter()
        .find(|c| c.text == "zz-context")
        .unwrap();
    assert_eq!(candidate.description.as_deref(), root.to_str());
}

#[tokio::test]
async fn git_redirect_targets_use_shell_directory_even_with_invalid_git_directory() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("repo")).unwrap();
    fs::write(dir.path().join("out_shell"), "").unwrap();
    fs::write(dir.path().join("repo/out_git"), "").unwrap();
    let engine = engine();
    for input in ["git -C repo status > out", "git -C missing status > out"] {
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;
        assert_eq!(result.disposition, CompletionDisposition::Allowed);
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            vec!["out_shell"],
            "{input}"
        );
    }
}
