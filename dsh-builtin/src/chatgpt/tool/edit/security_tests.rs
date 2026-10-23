use super::*;
use crate::test_support::TestShellProxy;
use std::fs;
use std::{
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};
use tempfile::tempdir;

fn proxy(root: &Path) -> TestShellProxy {
    TestShellProxy {
        current_dir: root.to_owned(),
        confirm_result: true,
        ..TestShellProxy::default()
    }
}
fn edit(path: &str, p: &mut TestShellProxy) -> Result<String, String> {
    run(&json!({"path":path,"contents":"approved"}).to_string(), p)
}

#[test]
fn dangling_final_and_parent_links_never_create_their_targets() {
    for parent in [false, true] {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let missing = outside.path().join("missing");
        symlink(&missing, dir.path().join("link")).unwrap();
        let mut p = proxy(dir.path());
        let result = edit(if parent { "link/file" } else { "link" }, &mut p);
        assert!(result.is_err(), "{result:?}");
        assert!(!missing.exists());
        assert_eq!(p.confirm_calls, 0);
    }
}

#[test]
fn approval_time_replacements_and_new_files_are_conflicts() {
    for mutation in [
        "final-symlink",
        "parent-symlink",
        "final-inode",
        "parent-inode",
        "delete",
        "new-file",
    ] {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir(dir.path().join("parent")).unwrap();
        let file = dir.path().join("parent/file");
        if mutation != "new-file" {
            fs::write(&file, "original").unwrap();
        }
        fs::write(outside.path().join("file"), "outside sentinel").unwrap();
        let root = dir.path().to_owned();
        let other = outside.path().to_owned();
        let mut p = proxy(dir.path());
        p.approval_callback = Some(Box::new(move || {
            let file = root.join("parent/file");
            match mutation {
                "final-symlink" => {
                    fs::remove_file(&file).unwrap();
                    symlink(other.join("file"), file).unwrap();
                }
                "parent-symlink" => {
                    fs::rename(root.join("parent"), root.join("saved")).unwrap();
                    symlink(&other, root.join("parent")).unwrap();
                }
                "parent-inode" => {
                    fs::rename(root.join("parent"), root.join("saved")).unwrap();
                    fs::create_dir(root.join("parent")).unwrap();
                    fs::write(file, "other writer").unwrap();
                }
                "final-inode" => {
                    fs::write(root.join("replacement"), "original").unwrap();
                    fs::rename(root.join("replacement"), file).unwrap();
                }
                "delete" => fs::remove_file(file).unwrap(),
                _ => fs::write(file, "other writer").unwrap(),
            }
        }));
        let result = edit("parent/file", &mut p);
        assert!(result.is_err(), "{mutation}: {result:?}");
        assert_eq!(
            fs::read_to_string(outside.path().join("file")).unwrap(),
            "outside sentinel"
        );
        match mutation {
            "delete" => assert!(!file.exists()),
            "new-file" | "parent-inode" => {
                assert_eq!(fs::read_to_string(file).unwrap(), "other writer")
            }
            "final-inode" => assert_eq!(fs::read_to_string(file).unwrap(), "original"),
            _ => {}
        }
    }
}

#[test]
fn hardlinks_are_rejected_without_truncating_the_linked_file() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("original"), "sentinel").unwrap();
    fs::hard_link(dir.path().join("original"), dir.path().join("alias")).unwrap();
    assert!(edit("alias", &mut proxy(dir.path())).is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join("original")).unwrap(),
        "sentinel"
    );
}

#[test]
fn write_only_file_keeps_its_mode_and_has_an_unavailable_preview() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("file");
    fs::write(&file, "unreadable preimage").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o200)).unwrap();
    let mut p = proxy(dir.path());
    let result = edit("file", &mut p);
    let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    result.unwrap();
    assert_eq!(mode, 0o200);
    assert_eq!(fs::read_to_string(file).unwrap(), "approved");
    assert!(
        p.approval_messages[0].contains("existing content unavailable"),
        "{:?}",
        p.approval_messages
    );
}

#[test]
fn denial_creates_no_parents_and_preserves_existing_files() {
    let dir = tempdir().unwrap();
    let mut p = proxy(dir.path());
    p.confirm_result = false;
    assert!(
        edit("new/nested/file", &mut p)
            .unwrap()
            .contains("cancelled")
    );
    assert!(!dir.path().join("new").exists());
    fs::write(dir.path().join("file"), "original").unwrap();
    edit("file", &mut p).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("file")).unwrap(),
        "original"
    );
}

#[test]
fn normal_internal_symlinks_and_execute_only_parents_keep_working() {
    let dir = tempdir().unwrap();
    let parent = dir.path().join("private");
    fs::create_dir(&parent).unwrap();
    fs::write(parent.join("file"), "original").unwrap();
    symlink("private/file", dir.path().join("alias")).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o111)).unwrap();
    let result = edit("alias", &mut proxy(dir.path()));
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    result.unwrap();
    assert_eq!(fs::read_to_string(parent.join("file")).unwrap(), "approved");
}

#[test]
fn fifo_edits_reject_promptly_in_a_bounded_child() {
    use std::{
        process::{Command, Stdio},
        time::Duration,
    };
    use wait_timeout::ChildExt;
    const CHILD: &str = "DOGESH_EDIT_FIFO_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let dir = tempdir().unwrap();
        nix::unistd::mkfifo(
            &dir.path().join("fifo"),
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        assert!(edit("fifo", &mut proxy(dir.path())).is_err());
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "chatgpt::tool::edit::security_tests::fifo_edits_reject_promptly_in_a_bounded_child",
        ])
        .env(CHILD, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let status = child.wait_timeout(Duration::from_secs(3)).unwrap();
    if status.is_none() {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    assert!(
        status.is_some_and(|status| status.success()),
        "FIFO edit must reject without blocking"
    );
}

#[test]
fn creation_modes_follow_foreground_umask_and_restricted_permissions() {
    use std::process::{Command, Stdio};
    const CHILD: &str = "DOGESH_EDIT_UMASK_CHILD";
    if std::env::var_os(CHILD).is_some() {
        nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o027));
        let dir = tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        edit("ordinary/nested/file", &mut proxy(&root)).unwrap();
        assert_eq!(
            fs::metadata(root.join("ordinary/nested/file"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(
            fs::metadata(root.join("ordinary/nested"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        crate::agent::files::write(&root.join("restricted/nested/file"), "private").unwrap();
        assert_eq!(
            fs::metadata(root.join("restricted/nested/file"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(root.join("restricted/nested"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        return;
    }
    assert!(Command::new(std::env::current_exe().unwrap())
        .args(["--exact","chatgpt::tool::edit::security_tests::creation_modes_follow_foreground_umask_and_restricted_permissions"])
        .env(CHILD,"1").stdin(Stdio::null()).stdout(Stdio::null()).status().unwrap().success());
}

#[test]
fn large_writes_keep_no_size_cap_and_restricted_reads_stay_bounded() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let contents = "x".repeat(16 * 1024 * 1024 + 1);
    let result = run(
        &json!({"path":"large","contents":contents}).to_string(),
        &mut proxy(&root),
    );
    result.unwrap();
    assert_eq!(
        fs::metadata(root.join("large")).unwrap().len(),
        16 * 1024 * 1024 + 1
    );
    crate::agent::files::write(&root.join("restricted"), &contents).unwrap();
    assert_eq!(
        fs::metadata(root.join("restricted")).unwrap().len(),
        16 * 1024 * 1024 + 1
    );
    assert!(
        crate::agent::files::read(&root.join("restricted"))
            .unwrap_err()
            .to_string()
            .contains("16 MiB")
    );
}

#[test]
fn hardlink_added_during_approval_is_rejected() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file"), "original").unwrap();
    let root = dir.path().to_owned();
    let mut p = proxy(dir.path());
    p.approval_callback = Some(Box::new(move || {
        fs::hard_link(root.join("file"), root.join("second")).unwrap()
    }));
    assert!(edit("file", &mut p).is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join("second")).unwrap(),
        "original"
    );
}
