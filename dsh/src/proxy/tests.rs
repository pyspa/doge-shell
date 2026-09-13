//! Tests for `proxy/mod.rs`'s own free functions: direnv root matching and the y/N confirmation prompt.
use super::*;
use std::fs;

#[test]
fn direnv_allowance_requires_exact_root_match() {
    let dir = tempfile::tempdir().unwrap();
    let allowed = dir.path().join("repo");
    let child = allowed.join("subproject");
    fs::create_dir_all(&child).unwrap();

    assert!(is_same_direnv_root(&allowed, &allowed));
    assert!(
        !is_same_direnv_root(&child, &allowed),
        "allow-direnv should not implicitly trust nested project roots"
    );
}

#[test]
fn confirmation_accepts_only_single_y() {
    assert!(confirmation_is_yes("y\n"));
    assert!(confirmation_is_yes("Y\r\n"));
    assert!(confirmation_is_yes(" y "));

    assert!(!confirmation_is_yes(""));
    assert!(!confirmation_is_yes("\n"));
    assert!(!confirmation_is_yes("n\n"));
    assert!(!confirmation_is_yes("yes\n"));
    assert!(!confirmation_is_yes("1\n"));
}

#[test]
fn confirmation_reads_tty_only_when_stdin_is_terminal() {
    assert!(should_read_confirmation_from_tty(true));
    assert!(!should_read_confirmation_from_tty(false));
}
