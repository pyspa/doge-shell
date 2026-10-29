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
