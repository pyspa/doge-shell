use super::*;
use crate::{shell_capabilities::ApprovalDecision, test_support::TestShellProxy};
use std::fs;
use tempfile::tempdir;

const ORIGINAL: &str = "alpha\nbeta\n";
const ARGS: &str = r#"{"path":"a.txt","old_string":"alpha","new_string":"delta"}"#;

#[test]
fn approval_window_changes_are_conflicts_including_allow_always() {
    for always in [false, true] {
        for mutation in ["other-line", "delete", "same-bytes-new-inode"] {
            let dir = tempdir().unwrap();
            let path = dir.path().join("a.txt");
            fs::write(&path, ORIGINAL).unwrap();
            let target = path.clone();
            let mut proxy = TestShellProxy {
                current_dir: dir.path().to_path_buf(),
                approval_decision: Some(if always {
                    ApprovalDecision::AllowAlways
                } else {
                    ApprovalDecision::Allow
                }),
                approval_callback: Some(Box::new(move || match mutation {
                    "other-line" => fs::write(&target, "alpha\nuser edit\n").unwrap(),
                    "delete" => fs::remove_file(&target).unwrap(),
                    _ => {
                        let replacement = target.with_extension("new");
                        fs::write(&replacement, ORIGINAL).unwrap();
                        fs::rename(replacement, &target).unwrap();
                    }
                })),
                ..TestShellProxy::default()
            };
            let err = run(ARGS, &mut proxy).unwrap_err();
            assert!(
                err.contains("conflict"),
                "{mutation}, always={always}: {err}"
            );
            match mutation {
                "delete" => assert!(!path.exists()),
                "other-line" => {
                    assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\nuser edit\n")
                }
                _ => assert_eq!(fs::read_to_string(&path).unwrap(), ORIGINAL),
            }
        }
    }
}

#[test]
fn allow_always_then_cached_approval_still_replaces_current_contents() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, ORIGINAL).unwrap();
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        approval_decision: Some(ApprovalDecision::AllowAlways),
        ..TestShellProxy::default()
    };
    run(ARGS, &mut proxy).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "delta\nbeta\n");
    fs::write(&path, "alpha\nnew line\n").unwrap();
    proxy.approval_decision = Some(ApprovalDecision::Deny);
    run(ARGS, &mut proxy).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "delta\nnew line\n");
    assert_eq!(proxy.confirm_calls, 1);
}

#[test]
fn replacement_preserves_mode_and_execute_only_parent_access() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let parent = dir.path().join("private");
    fs::create_dir(&parent).unwrap();
    let path = parent.join("a.txt");
    fs::write(&path, ORIGINAL).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o111)).unwrap();
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        confirm_result: true,
        ..TestShellProxy::default()
    };
    let result = run(&ARGS.replace("a.txt", "private/a.txt"), &mut proxy);
    // Restore directory permissions even if the replacement regresses.
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    result.unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "delta\nbeta\n");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn read_errors_are_not_treated_as_empty_preimages() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, ORIGINAL).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o200)).unwrap();
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        confirm_result: true,
        ..TestShellProxy::default()
    };
    let result = run(ARGS, &mut proxy);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(result.unwrap_err().contains("failed to read"));
    assert_eq!(proxy.confirm_calls, 0);
    assert_eq!(fs::read_to_string(&path).unwrap(), ORIGINAL);
    fs::write(&path, [0xff]).unwrap();
    assert!(
        run(ARGS, &mut proxy)
            .unwrap_err()
            .contains("failed to read")
    );
    assert_eq!(fs::read(&path).unwrap(), [0xff]);
}

#[test]
fn existing_in_workspace_symlink_remains_supported() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("target.txt");
    fs::write(&path, ORIGINAL).unwrap();
    std::os::unix::fs::symlink("target.txt", dir.path().join("a.txt")).unwrap();
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        confirm_result: true,
        ..TestShellProxy::default()
    };
    run(ARGS, &mut proxy).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "delta\nbeta\n");
    assert!(
        fs::symlink_metadata(dir.path().join("a.txt"))
            .unwrap()
            .is_symlink()
    );
}
