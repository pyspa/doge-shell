//! Filesystem completion with isolated process cwd and deterministic fixtures.
use doge_shell::completion::generators::filesystem::FileSystemGenerator;
use doge_shell::completion::{
    Candidate, CompletionType, path_completion_path, path_completion_path_sync,
};
use std::path::PathBuf;

fn isolated(name: &str, test: impl FnOnce()) {
    if std::env::var("DSH_COMPLETION_LISTING_TEST").as_deref() == Ok(name) {
        test();
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env("DSH_COMPLETION_LISTING_TEST", name)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn parent_listing_does_not_replace_child_listing() {
    isolated("parent_listing_does_not_replace_child_listing", || {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let inside = child.join("inside.txt");
        std::fs::write(&inside, "").unwrap();
        path_completion_path_sync(dir.path().into()).unwrap();
        let candidates =
            FileSystemGenerator::generate_file_candidates(&format!("{}/", child.display()))
                .unwrap();
        assert!(
            candidates
                .iter()
                .any(|c| c.text == inside.to_string_lossy()),
            "{candidates:?}"
        );
        assert!(
            candidates
                .iter()
                .all(|c| std::path::Path::new(&c.text).exists()),
            "phantom: {candidates:?}"
        );
    });
}

#[test]
fn background_listing_does_not_reuse_parent_cache() {
    isolated("background_listing_does_not_reuse_parent_cache", || {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let inside = child.join("inside.txt");
        std::fs::write(&inside, "").unwrap();
        path_completion_path_sync(dir.path().into()).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        doge_shell::completion::set_completion_notifier(tx);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut candidates = path_completion_path(child.clone()).unwrap();
            if candidates.is_empty() {
                tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
                candidates = path_completion_path(child).unwrap();
            }
            assert!(
                candidates
                    .iter()
                    .any(|c| matches!(c,Candidate::Path(p) if p == &inside.to_string_lossy())),
                "{candidates:?}"
            );
        });
    });
}

#[test]
fn relative_listing_is_scoped_to_current_directory() {
    isolated("relative_listing_is_scoped_to_current_directory", || {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(first.path().join("first.txt"), "").unwrap();
        std::fs::write(second.path().join("second.txt"), "").unwrap();
        std::env::set_current_dir(first.path()).unwrap();
        path_completion_path_sync(PathBuf::from(".")).unwrap();
        std::env::set_current_dir(second.path()).unwrap();
        let candidates = path_completion_path_sync(PathBuf::from(".")).unwrap();
        assert!(
            candidates
                .iter()
                .any(|c| matches!(c,Candidate::Path(p) if p == "./second.txt")),
            "{candidates:?}"
        );
        assert!(
            !candidates
                .iter()
                .any(|c| matches!(c,Candidate::Path(p) if p == "./first.txt"))
        );
    });
}

#[test]
fn literal_trailing_backslash_is_not_directory_syntax() {
    isolated("literal_trailing_backslash_is_not_directory_syntax", || {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tail\\");
        std::fs::write(&file, "").unwrap();
        let prefix = dir.path().join("tail").to_string_lossy().into_owned();
        let candidates = FileSystemGenerator::generate_file_candidates(&prefix).unwrap();
        assert!(
            candidates
                .iter()
                .any(|c| c.text == file.to_string_lossy()
                    && c.completion_type == CompletionType::File),
            "{candidates:?}"
        );
        assert!(
            FileSystemGenerator::generate_directory_candidates(&prefix)
                .unwrap()
                .is_empty()
        );
    });
}

#[test]
fn relative_ghost_keeps_input_spelling() {
    isolated("relative_ghost_keeps_input_spelling", || {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("foo.txt"), "").unwrap();
        std::fs::write(dir.path().join("界é file.txt"), "").unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        path_completion_path_sync(PathBuf::from(".")).unwrap();
        for (input, start, expected) in [
            ("cat fo", 4, "cat foo.txt"),
            ("cat ./fo", 4, "cat ./foo.txt"),
            ("cat \"界é f", 4, "cat \"界é file.txt"),
            (r"cat 界é\ f", 4, r"cat 界é\ file.txt"),
        ] {
            let span = pest::Span::new(input, start, input.len()).unwrap();
            assert_eq!(
                doge_shell::repl::completion::complete_argument_word(input, &span, &input[start..]),
                Some(expected.into()),
                "{input}"
            );
        }
    });
}

#[test]
fn background_request_keeps_original_cwd_and_cache_spelling() {
    isolated(
        "background_request_keeps_original_cwd_and_cache_spelling",
        || {
            let first = tempfile::tempdir().unwrap();
            let second = tempfile::tempdir().unwrap();
            std::fs::write(first.path().join("first.txt"), "").unwrap();
            std::fs::write(second.path().join("second.txt"), "").unwrap();
            std::env::set_current_dir(first.path()).unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            doge_shell::completion::set_completion_notifier(tx);
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            assert!(path_completion_path(PathBuf::from(".")).unwrap().is_empty());
            // The spawned async task cannot run until the receive yields.
            std::env::set_current_dir(second.path()).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv()).await.unwrap().unwrap();
            let second_listing = path_completion_path_sync(PathBuf::from(".")).unwrap();
            assert!(second_listing.iter().any(|c| matches!(c,Candidate::Path(p) if p == "./second.txt")), "{second_listing:?}");
            std::env::set_current_dir(first.path()).unwrap();
            let first_listing = path_completion_path_sync(PathBuf::from(".")).unwrap();
            assert!(first_listing.iter().any(|c| matches!(c,Candidate::Path(p) if p == "./first.txt")), "{first_listing:?}");
            let absolute = path_completion_path_sync(first.path().to_path_buf()).unwrap();
            assert!(absolute.iter().any(|c| matches!(c,Candidate::Path(p) if p == &first.path().join("first.txt").to_string_lossy())));
        });
        },
    );
}

#[test]
fn relative_cached_membership_matches_listing_spelling() {
    isolated(
        "relative_cached_membership_matches_listing_spelling",
        || {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("foo.txt"), "").unwrap();
            std::env::set_current_dir(dir.path()).unwrap();
            path_completion_path_sync(PathBuf::from(".")).unwrap();
            for spelling in ["foo.txt", "./foo.txt"] {
                assert!(
                    doge_shell::completion::is_path_cached(std::path::Path::new(spelling)),
                    "{spelling}"
                );
            }
            assert!(!doge_shell::completion::is_path_cached(
                std::path::Path::new("missing.txt")
            ));
        },
    );
}
