use super::*;
use crate::completion::{commands::deduplicate_candidates, path::path_completion_path_sync};
use skim::SkimItem;

#[test]
fn backslash_identity_legacy_files_and_command_preference() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["file", "file\\", "file\\\\"] {
        std::fs::write(dir.path().join(name), "").unwrap();
    }
    for name in ["dir", "dir\\", "dir\\\\"] {
        std::fs::create_dir(dir.path().join(name)).unwrap();
    }
    let collected = path_completion_path_sync(dir.path().to_path_buf()).unwrap();
    assert_eq!(collected.len(), 6);
    assert_eq!(deduplicate_candidates(collected).len(), 6);
    let merged = deduplicate_candidates(vec![
        Candidate::Item("name\\".into(), "(file)".into()),
        Candidate::Item("name".into(), "(command)".into()),
        Candidate::Item("./name\\".into(), "(command)".into()),
    ]);
    assert_eq!(merged.len(), 2);
    assert!(
        merged
            .iter()
            .all(|c| matches!(c, Candidate::Item(_, desc) if desc == "(command)"))
    );
}

#[test]
fn backslash_identity_integrated_types_ranking_and_slash_aliases() {
    let engine = IntegratedCompletionEngine::new(Environment::new_isolated());
    for kind in [
        CandidateType::File,
        CandidateType::Directory,
        CandidateType::HomeFile,
        CandidateType::HomeDirectory,
        CandidateType::Argument,
    ] {
        let candidate = |text: &str, priority| EnhancedCandidate {
            text: text.into(),
            description: None,
            candidate_type: kind.clone(),
            priority,
        };
        let result = engine.deduplicate_and_sort(
            vec![
                candidate("~/名 space", 10),
                candidate("~/名 space\\", 20),
                candidate("~/名 space\\\\", 30),
                candidate("~/名 space\\", 40),
            ],
            50,
            None,
            None,
        );
        assert_eq!(result.len(), 3, "{kind:?}: {result:?}");
        assert_eq!(result[0].text, "~/名 space\\");
        assert_eq!(result[0].priority, 40);
        let result = engine.deduplicate_and_sort(
            vec![
                candidate("dir\\/", 10),
                candidate("dir\\", 20),
                candidate("dir/", 30),
            ],
            50,
            None,
            None,
        );
        assert_eq!(result.len(), 2, "{kind:?}: {result:?}");
    }
}

#[tokio::test]
async fn backslash_identity_public_completion_and_insertion() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for name in ["file", "file\\", "file\\\\"] {
        std::fs::write(root.join(name), "").unwrap();
    }
    for name in ["dir", "dir\\", "dir\\\\"] {
        std::fs::create_dir(root.join(name)).unwrap();
    }
    let env = Environment::new_isolated();
    env.write()
        .set_shell_var("DOGESH_EXTERNAL_COMPLETER".into(), String::new());
    env.write()
        .set_shell_var("DOGESH_COMPLETION_FISH_FALLBACK".into(), "off".into());
    let mut engine = IntegratedCompletionEngine::new(env);
    engine.initialize_command_completion().unwrap();
    for (command, prefix) in [("cat", "fi"), ("cd", "di")] {
        for quote in ["", "\""] {
            let token = format!("{quote}{}/{prefix}", root.display());
            let input = format!("{command} {token}");
            for _ in 0..2 {
                let result = engine
                    .complete(&input, input.chars().count(), &root, 50, None)
                    .await;
                assert_eq!(
                    result.candidates.len(),
                    3,
                    "{input}: {:?}",
                    result.candidates
                );
                let formatted = crate::completion::shell_path::format_candidates_for_token(
                    engine.to_candidates(result.candidates),
                    Some(&token),
                );
                for candidate in formatted {
                    let raw = candidate.get_display_name();
                    assert_eq!(
                        crate::completion::shell_path::normalize_path_token(&candidate.output()),
                        raw
                    );
                }
            }
        }
    }
}
