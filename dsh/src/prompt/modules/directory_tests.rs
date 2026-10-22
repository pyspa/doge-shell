use super::*;

fn render(current_dir: &Path, git_root: Option<&Path>) -> String {
    let context = PromptContext {
        current_dir,
        project_root: None,
        git_root,
        git_status: None,
        has_rust_project: false,
        has_node_project: false,
        has_python_project: false,
        has_go_project: false,
        rust_version: None,
        rust_source: None,
        node_version: None,
        node_source: None,
        python_version: None,
        python_source: None,
        go_version: None,
        go_source: None,
        k8s_context: None,
        k8s_namespace: None,
        aws_profile: None,
        docker_context: None,
        last_exit_status: 0,
        last_duration: None,
    };
    DirectoryModule::new().render(&context).unwrap()
}

#[test]
fn cwd_safety_visible_directory_escapes_controls_before_styling() {
    let root = Path::new("/owned/repo\x07\x1b]52;c;payload\x1b\\");
    let cwd = root.join("日本\r\n\t\u{85}\u{9c}");
    let styled = render(&cwd, Some(root));
    let plain = dsh_types::ansi::strip_ansi(&styled);
    assert!(!plain.chars().any(char::is_control));
    assert_eq!(
        plain,
        r"repo\u{7}\u{1b}]52;c;payload\u{1b}\/日本\r\n\t\u{85}\u{9c}"
    );
    assert_eq!(styled, plain.clone().cyan().to_string());
    assert_eq!(
        crate::input::display_width(&styled),
        unicode_width::UnicodeWidthStr::width(plain.as_str())
    );
    assert!(cwd.to_string_lossy().contains('\x07'));
}

#[test]
fn cwd_safety_visible_non_git_and_unterminated_escape() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root
        .path()
        .join("a\x07\r\n\u{9c}\x1b]".to_owned() + &"x".repeat(5000));
    let output = render(&cwd, None);
    assert!(!output.chars().any(char::is_control));
    assert!(output.ends_with(&"x".repeat(5000)));
}

#[test]
fn cwd_safety_directory_preserves_abbreviation_unicode_and_git_paths() {
    assert_eq!(
        format_prompt_path(
            Path::new("/home/test/日本"),
            None,
            Some(Path::new("/home/test"))
        ),
        ("~/日本".into(), false)
    );
    assert_eq!(
        format_prompt_path(
            Path::new("/work/repo/日本"),
            Some(Path::new("/work/repo")),
            None
        ),
        ("repo/日本".into(), true)
    );
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    let output = render(root.path(), None);
    assert_eq!(output, root.path().file_name().unwrap().to_string_lossy());
}
