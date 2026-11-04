use super::*;

#[test]
fn cached_ghost_listings_share_the_directory_buffer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("file.txt"), "").unwrap();
    path_completion_path_sync(dir.path().to_path_buf()).unwrap();

    let first = path_completion_path_with_context(dir.path().to_path_buf(), None).unwrap();
    let second = path_completion_path_with_context(dir.path().to_path_buf(), None).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert!(
        std::ptr::eq(first.as_ptr(), second.as_ptr()),
        "warm ghost lookups must borrow one cached directory buffer, not copy it"
    );
}

#[test]
fn ghost_selection_keeps_shortest_then_lexicographic_order() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["applelong", "apz", "apa", "ap界"] {
        std::fs::write(dir.path().join(name), "").unwrap();
    }
    std::fs::create_dir(dir.path().join("apdir")).unwrap();
    path_completion_path_sync(dir.path().to_path_buf()).unwrap();

    for (prefix, only_dirs, expected) in [
        ("ap", false, Some("apa")),
        ("ap", true, Some("apdir/")),
        ("ap界", false, Some("ap界")),
        ("missing", false, None),
    ] {
        let input = dir.path().join(prefix).display().to_string();
        let expected = expected.map(|name| format!("{}/{name}", dir.path().display()));
        assert_eq!(
            path_completion_prefix_strict(&input, only_dirs).unwrap(),
            expected
        );
    }
}

#[test]
fn directory_scan_preserves_types_links_and_sorted_spelling() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("z-file"), "").unwrap();
    std::fs::create_dir(dir.path().join("a-dir")).unwrap();
    symlink("a-dir", dir.path().join("b-relative-dir")).unwrap();
    symlink(dir.path().join("a-dir"), dir.path().join("c-absolute-dir")).unwrap();
    symlink("z-file", dir.path().join("d-file-link")).unwrap();
    symlink("missing", dir.path().join("e-broken")).unwrap();
    symlink("f-loop", dir.path().join("f-loop")).unwrap();
    let request = DirectoryRequest::new(dir.path().to_path_buf()).unwrap();
    let names: Vec<_> = scan_dir_candidates(request)
        .unwrap()
        .into_iter()
        .map(|c| {
            let Candidate::Path(path) = c else {
                panic!("expected path")
            };
            path.strip_prefix(&format!("{}/", dir.path().display()))
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        names,
        [
            "a-dir/",
            "b-relative-dir/",
            "c-absolute-dir/",
            "d-file-link",
            "e-broken",
            "f-loop",
            "z-file"
        ]
    );
}

#[test]
fn directory_scan_propagates_missing_directory_error() {
    let dir = tempfile::tempdir().unwrap();
    let request = DirectoryRequest::new(dir.path().join("missing")).unwrap();
    assert!(scan_dir_candidates(request).is_err());
}

#[test]
fn directory_scan_keeps_unreadable_directory_entry() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let blocked = dir.path().join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o0)).unwrap();
    let result = scan_dir_candidates(DirectoryRequest::new(dir.path().to_path_buf()).unwrap());
    let unreadable = scan_dir_candidates(DirectoryRequest::new(blocked.clone()).unwrap());
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        result.unwrap(),
        vec![Candidate::Path(format!("{}/", blocked.display()))]
    );
    assert!(
        unreadable.is_err(),
        "run as an unprivileged user to exercise EACCES"
    );
}

#[test]
fn synchronous_internal_listings_share_the_cached_buffer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("file"), "").unwrap();
    let first = path_completion_path_sync_shared(dir.path().to_path_buf()).unwrap();
    let second = path_completion_path_sync_shared(dir.path().to_path_buf()).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn directory_scan_keeps_links_to_inaccessible_targets_as_files() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let blocked = dir.path().join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::create_dir(blocked.join("target")).unwrap();
    symlink("blocked/target", dir.path().join("link")).unwrap();
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o0)).unwrap();
    let result = scan_dir_candidates(DirectoryRequest::new(dir.path().to_path_buf()).unwrap());
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = result.unwrap();
    assert!(result.contains(&Candidate::Path(format!("{}/link", dir.path().display()))));
}

#[test]
fn vanished_file_entry_remains_a_non_directory() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    std::fs::write(&file, "").unwrap();
    let entry = read_dir(dir.path()).unwrap().next().unwrap().unwrap();
    std::fs::remove_file(file).unwrap();
    assert!(!entry_is_dir(&entry));
}

#[test]
fn readable_but_unsearchable_parent_preserves_non_directory_spelling() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent");
    std::fs::create_dir(&parent).unwrap();
    std::fs::create_dir(parent.join("child")).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o400)).unwrap();
    let result = scan_dir_candidates(DirectoryRequest::new(parent.clone()).unwrap());
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        result.unwrap(),
        vec![Candidate::Path(format!("{}/child", parent.display()))]
    );
}

#[test]
fn shared_listing_remains_available_after_cache_clear_or_expiry() {
    let cache = CompletionCache::new(Duration::ZERO);
    let held = cache.set_shared("directory".into(), vec![Candidate::Path("file".into())]);
    assert!(cache.get_entry("directory").is_none());
    cache.clear();
    assert_eq!(held.as_ref(), &vec![Candidate::Path("file".into())]);
}
