use super::*;

#[test]
fn bare_relative_directory_prefix_lists_children() {
    const CHILD: &str = "DOGESH_RELATIVE_PREFIX_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["dir/folder", "nested/deep", "empty", "space dir"] {
            std::fs::create_dir_all(temp.path().join(name)).unwrap();
        }
        for name in [
            "dir/child.txt",
            "nested/deep/leaf",
            "space dir/child",
            "cwd-only",
        ] {
            std::fs::write(temp.path().join(name), "").unwrap();
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "completion::path::relative_tests::bare_relative_directory_prefix_lists_children",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for _ in 0..2 {
        for (input, dirs, expected) in [
            ("dir/", false, Some("dir/folder/")),
            ("./dir/", false, Some("./dir/folder/")),
            ("dir/ch", false, Some("dir/child.txt")),
            ("dir/", true, Some("dir/folder/")),
            ("nested/deep/", false, Some("nested/deep/leaf")),
            ("empty/", false, None),
        ] {
            assert_eq!(
                path_completion_prefix_strict(input, dirs)
                    .unwrap()
                    .as_deref(),
                expected,
                "{input}, dirs={dirs}"
            );
        }
        assert!(path_completion_prefix_strict("missing/", false).is_err());
        assert!(path_completion_prefix_strict("./missing/", false).is_err());
        for (raw, expected) in [
            ("dir/", "dir/folder/"),
            ("\"space dir/", "\"space dir/child"),
            ("space\\ dir/", "space\\ dir/child"),
        ] {
            assert_eq!(
                path_completion_prefix_for_shell_token(raw, false)
                    .unwrap()
                    .as_deref(),
                Some(expected),
                "{raw}"
            );
        }
        let absolute = std::env::current_dir().unwrap().join("dir");
        assert_eq!(
            path_completion_prefix_strict(&format!("{}/", absolute.display()), true).unwrap(),
            Some(format!("{}/folder/", absolute.display()))
        );
    }
}
