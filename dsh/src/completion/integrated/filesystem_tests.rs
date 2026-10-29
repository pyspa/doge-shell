use super::*;
use std::fs;

fn engine() -> IntegratedCompletionEngine {
    let environment = Environment::new_isolated();
    environment
        .write()
        .set_shell_var("DOGESH_EXTERNAL_COMPLETER".into(), String::new());
    environment
        .write()
        .set_shell_var("DOGESH_COMPLETION_FISH_FALLBACK".into(), "off".into());
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine.initialize_command_completion().unwrap();
    engine
}

#[tokio::test]
async fn filesystem_cache_follows_request_directory() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    fs::write(a.join("audit_apple"), "").unwrap();
    fs::write(b.join("audit_banana"), "").unwrap();
    let engine = engine();
    let input = "cat audit_";
    for (cwd, expected) in [
        (&a, "audit_apple"),
        (&b, "audit_banana"),
        (&a, "audit_apple"),
    ] {
        let result = engine.complete(input, input.len(), cwd, 50, None).await;
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            vec![expected]
        );
    }
}

#[tokio::test]
async fn filesystem_completion_follows_logical_home_changes() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine();
    for name in ["first", "second"] {
        let home = dir.path().join(name);
        fs::create_dir(&home).unwrap();
        fs::write(home.join(format!("audit_{name}.txt")), "").unwrap();
        fs::create_dir(home.join(format!("audit_{name}_dir"))).unwrap();
        engine
            .environment
            .write()
            .set_shell_var("HOME".into(), home.display().to_string());
        for input in ["cat ~/audit_", "cd ~/audit_", "unknown-command ~/audit_"] {
            let result = engine
                .complete(input, input.len(), dir.path(), 50, None)
                .await;
            let texts: Vec<_> = result
                .candidates
                .iter()
                .map(|c| c.text.trim_end_matches('/'))
                .collect();
            assert!(
                texts.contains(&format!("~/audit_{name}_dir").as_str()),
                "{input}: {texts:?}"
            );
            assert_eq!(
                texts.len(),
                if input.starts_with("cd ") { 1 } else { 2 },
                "{input}: {texts:?}"
            );
            assert!(result.candidates.iter().all(|c| matches!(
                c.candidate_type,
                CandidateType::HomeFile | CandidateType::HomeDirectory
            )));
        }
    }
}

#[test]
fn filesystem_completion_finds_exact_names_after_large_listing() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..520 {
        fs::write(dir.path().join(format!("file_{i:04}.txt")), "").unwrap();
        fs::create_dir(dir.path().join(format!("dir_{i:04}"))).unwrap();
    }
    for i in 0..520 {
        let file = dir
            .path()
            .join(format!("file_{i:04}.txt"))
            .display()
            .to_string();
        let candidates = FileSystemGenerator::generate_file_candidates_with_filter(
            &file,
            Some(&vec!["txt".into()]),
        )
        .unwrap();
        assert!(candidates.iter().any(|c| c.text == file), "missing {file}");
        let directory = dir.path().join(format!("dir_{i:04}")).display().to_string();
        let candidates = FileSystemGenerator::generate_directory_candidates(&directory).unwrap();
        assert!(
            candidates
                .iter()
                .any(|c| c.text.trim_end_matches('/') == directory),
            "missing {directory}"
        );
    }
}
