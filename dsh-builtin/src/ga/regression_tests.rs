use super::*;

fn runtime(dir: &std::path::Path) -> CommandRuntimeSnapshot {
    let paths = std::env::split_paths(&std::env::var_os("PATH").unwrap()).collect();
    let env = [
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
    ]
    .into_iter()
    .collect();
    CommandRuntimeSnapshot::new(paths, env, dir.to_path_buf())
}

#[test]
fn selected_names_are_literal_paths() {
    for name in ["-A", "a*.txt", ":(glob)*"] {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime(dir.path());
        assert!(
            rt.std_command("git")
                .unwrap()
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(dir.path().join(name), "selected").unwrap();
        std::fs::write(dir.path().join("another.txt"), "unselected").unwrap();
        let output = stage_files(&rt, &[name.into()]).unwrap();
        assert!(output.status.success(), "{:?}", output);
        let indexed = rt
            .std_command("git")
            .unwrap()
            .args(["ls-files", "-z"])
            .output()
            .unwrap();
        assert_eq!(indexed.stdout, format!("{name}\0").as_bytes());
    }
}

#[test]
fn status_preserves_quoted_paths() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(dir.path());
    assert!(
        rt.std_command("git")
            .unwrap()
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    for name in ["space name.txt", "日本語.txt", "line\nbreak.txt"] {
        std::fs::write(dir.path().join(name), "data").unwrap();
    }
    let entries = get_git_status(&rt).unwrap();
    for name in ["space name.txt", "日本語.txt", "line\nbreak.txt"] {
        assert!(
            entries
                .iter()
                .any(|item| item.path == std::path::Path::new(name)),
            "missing {name:?}"
        );
    }
}

#[test]
fn stage_from_subdirectory_uses_repository_paths() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(dir.path());
    assert!(
        rt.std_command("git")
            .unwrap()
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/file.txt"), "data").unwrap();
    let nested = runtime(&dir.path().join("sub"));
    let output = stage_files(&nested, &["sub/file.txt".into()]).unwrap();
    assert!(output.status.success(), "{:?}", output);
}

#[test]
fn rename_and_non_utf8_paths_can_be_staged() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(dir.path());
    let git = |args: &[&str]| {
        let output = rt.std_command("git").unwrap().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    std::fs::write(dir.path().join("old"), "initial").unwrap();
    git(&["add", "old"]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-qm",
        "initial",
    ]);
    git(&["mv", "old", "new name"]);
    std::fs::write(dir.path().join("new name"), "changed").unwrap();
    let raw = PathBuf::from(OsString::from_vec(b"raw-\xff".to_vec()));
    std::fs::write(dir.path().join(&raw), "raw").unwrap();
    let entries = get_git_status(&rt).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .any(|item| item.path == std::path::Path::new("new name"))
    );
    assert!(entries.iter().any(|item| item.path == raw));
    let paths: Vec<_> = entries.iter().map(|item| item.path.clone()).collect();
    assert!(stage_files(&rt, &paths).unwrap().status.success());
    let output = rt
        .std_command("git")
        .unwrap()
        .args(["show", ":new name"])
        .output()
        .unwrap();
    assert_eq!(output.stdout, b"changed");
    let indexed = rt
        .std_command("git")
        .unwrap()
        .args(["ls-files", "-z"])
        .output()
        .unwrap();
    assert!(indexed.stdout.split(|b| *b == 0).any(|p| p == b"raw-\xff"));
}

#[test]
fn relative_git_directory_environment_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let base = runtime(dir.path());
    assert!(
        base.std_command("git")
            .unwrap()
            .args(["init", "--bare", "-q", "repo.git"])
            .status()
            .unwrap()
            .success()
    );
    std::fs::create_dir(dir.path().join("work")).unwrap();
    std::fs::write(dir.path().join("work/file.txt"), "data").unwrap();
    let mut env = base.child_env().clone();
    env.insert("GIT_DIR".into(), "repo.git".into());
    env.insert("GIT_WORK_TREE".into(), "work".into());
    let rt = CommandRuntimeSnapshot::new(
        base.command_search_paths().to_vec(),
        env,
        dir.path().to_path_buf(),
    );
    let entries = get_git_status(&rt).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        stage_files(&rt, &[entries[0].path.clone()])
            .unwrap()
            .status
            .success()
    );
}
