use super::*;
use std::fs;

fn linear_membership(candidates: &[Candidate], search: &str) -> bool {
    candidates.iter().any(|candidate| {
        matches!(candidate, Candidate::Path(raw)
            if raw.trim_end_matches(std::path::MAIN_SEPARATOR)
                == search.trim_end_matches(std::path::MAIN_SEPARATOR))
    })
}

#[test]
fn cached_membership_matches_linear_oracle_at_raw_sort_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let request = DirectoryRequest::new(dir.path().to_path_buf()).unwrap();
    // Stripping '/' would put "a" before "a!" even though raw "a/" sorts after it.
    let names = ["a/", "a!", "a-", "a.", "a0", "space dir/", "é", "界é", "z/"];
    for mask in 0..1 << names.len() {
        let mut candidates: Vec<_> = names
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, name)| Candidate::Path(format!("{}/{name}", dir.path().display())))
            .collect();
        candidates.sort();
        PATH_COMPLETION_CACHE.set(request.key.clone(), candidates.clone());
        for name in names
            .iter()
            .copied()
            .chain(["", "0-missing", "a+", "a1", "zz-missing"])
        {
            let query = dir.path().join(name);
            let expected = linear_membership(&candidates, &query.to_string_lossy());
            // An empty file_name is never a member, even if the listing is non-empty.
            assert_eq!(
                is_path_cached(&query),
                expected,
                "mask={mask} query={query:?}"
            );
        }
    }
}

#[test]
fn cached_membership_preserves_raw_files_directories_links_and_shell_spelling() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "a!",
        "a-",
        "a.",
        "a0",
        "界é file",
        "quote'\"file",
        "tail\\",
        "-option",
    ] {
        fs::write(dir.path().join(name), "").unwrap();
    }
    fs::create_dir(dir.path().join("a")).unwrap();
    symlink("a", dir.path().join("dir-link")).unwrap();
    symlink("a!", dir.path().join("file-link")).unwrap();
    symlink("absent", dir.path().join("broken-link")).unwrap();
    symlink("loop", dir.path().join("loop")).unwrap();
    let candidates = path_completion_path_sync(dir.path().to_path_buf()).unwrap();
    assert!(candidates.windows(2).all(|pair| pair[0] <= pair[1]));
    for candidate in &candidates {
        let Candidate::Path(raw) = candidate else {
            panic!("raw PATH listing invariant")
        };
        for spelling in [
            raw.clone(),
            raw.trim_end_matches('/').to_string(),
            format!("{}/", raw.trim_end_matches('/')),
        ] {
            assert!(is_path_cached(Path::new(&spelling)), "{spelling}");
        }
    }
    assert!(!is_path_cached(&dir.path().join("absent")));
    let plain = dir.path().join("界é file").to_string_lossy().into_owned();
    for raw in [
        plain.replace(' ', "\\ "),
        format!("'{plain}'"),
        format!("\"{plain}\""),
        format!("\"{plain}"),
    ] {
        assert!(is_path_cached_for_shell_token(&raw), "{raw}");
    }
    for raw in ["", "''", "\"\""] {
        assert!(!is_path_cached_for_shell_token(raw));
    }
}

#[test]
fn cached_membership_is_read_only_and_expires_without_extending_ttl() {
    let dir = tempfile::tempdir().unwrap();
    let removed = dir.path().join("removed");
    let added = dir.path().join("added");
    fs::write(&removed, "").unwrap();
    let request = DirectoryRequest::new(dir.path().to_path_buf()).unwrap();
    assert!(
        !is_path_cached(&removed),
        "cold cache must not stat or scan"
    );
    assert!(PATH_COMPLETION_CACHE.get_entry(&request.key).is_none());
    assert!(!PATH_COMPLETION_CACHE.is_pending(&request.key));
    path_completion_path_sync(dir.path().to_path_buf()).unwrap();
    fs::remove_file(&removed).unwrap();
    fs::write(&added, "").unwrap();
    assert!(
        is_path_cached(&removed),
        "membership describes cached listing, not disk"
    );
    assert!(!is_path_cached(&added));
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    while is_path_cached(&removed) {
        assert!(
            std::time::Instant::now() < deadline,
            "membership hits must not extend TTL"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!is_path_cached(&added), "expiry must not trigger a scan");
    assert!(PATH_COMPLETION_CACHE.get_entry(&request.key).is_none());
    assert!(!PATH_COMPLETION_CACHE.is_pending(&request.key));
    path_completion_path_sync(dir.path().to_path_buf()).unwrap();
    assert!(is_path_cached(&added));
    assert!(!is_path_cached(&removed));
}
