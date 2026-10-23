//! Legacy selection contracts and raw/preformatted insertion regressions.
use super::*;

#[tokio::test]
async fn legacy_lisp_lists_are_literal_but_direct_strings_are_preformatted() {
    use crate::{completion::AutoComplete, environment::Environment, shell::Shell};
    let environment = Environment::new_synthetic();
    environment.write().variable_state.paths.clear();
    let mut shell = Shell::new(environment);
    let callback = shell
        .lisp_engine
        .borrow()
        .run(r#"(lambda (input) "hello\ world")"#)
        .unwrap();
    let list_callback = shell
        .lisp_engine
        .borrow()
        .run(r#"(lambda (input) '("hello world"))"#)
        .unwrap();
    let repl = Repl::new(&mut shell);
    let callbacks = repl
        .shell
        .lisp_engine
        .borrow()
        .env
        .borrow()
        .autocompletion
        .clone();
    let mut input = Input::new(crate::input::InputConfig::default());
    input.reset("probe he".into());
    for (func, candidates) in [
        (None, Some(vec!["hello world".into()])),
        (Some(list_callback), None),
        (Some(callback), None),
    ] {
        *callbacks.borrow_mut() = vec![AutoComplete {
            target: "probe".into(),
            cmd: None,
            func,
            candidates,
        }];
        let selected =
            input_completion(&input, &repl, Some("he"), Some("he"), "", input.as_str()).await;
        assert_eq!(
            selected,
            CompletionSelection::Selected(r"hello\ world".into())
        );
    }
}

#[test]
fn legacy_inline_input_bypasses_literal_candidate_rendering() {
    let input = CompletionSelection::Input("x".into());
    assert_eq!(
        render_legacy_selection(input, Some(r"dir\ with\ space/fo")),
        CompletionSelection::Input("x".into())
    );
}

#[test]
fn default_framework_switches_to_skim_via_env() {
    let _guard = crate::test_env_lock();
    let original = std::env::var("DOGESH_COMPLETION_FRAMEWORK").ok();
    unsafe {
        std::env::set_var("DOGESH_COMPLETION_FRAMEWORK", "skim");
    }
    assert_eq!(
        super::default_completion_framework(),
        CompletionFrameworkKind::Skim
    );
    match original {
        Some(value) => unsafe {
            std::env::set_var("DOGESH_COMPLETION_FRAMEWORK", value);
        },
        None => unsafe {
            std::env::remove_var("DOGESH_COMPLETION_FRAMEWORK");
        },
    }
}

#[test]
fn strip_selected_marker_only_removes_star_space_prefix() {
    assert_eq!(super::strip_selected_marker("* main".to_string()), "main");
    // A candidate that merely starts with '*' must keep its text.
    assert_eq!(super::strip_selected_marker("*tar".to_string()), "*tar");
    assert_eq!(super::strip_selected_marker("*".to_string()), "*");
    assert_eq!(super::strip_selected_marker("plain".to_string()), "plain");
}

#[test]
fn test_fuzzy_file_completion_with_temp_dir() {
    use std::fs::File;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let dir_path = dir.path().to_str().unwrap();

    // Create some files
    File::create(dir.path().join("completion.rs")).unwrap();
    File::create(dir.path().join("command.rs")).unwrap();
    File::create(dir.path().join("cache.rs")).unwrap();
    File::create(dir.path().join("notes.txt")).unwrap();

    // Test fuzzy match "cmp" -> should find completion.rs
    let results = super::get_file_completions_with_filter_sync(dir_path, "", Some("cmp"));
    assert!(!results.is_empty());
    assert!(
        results
            .iter()
            .any(|c| matches!(c, Candidate::Item(name, _) if name.contains("completion.rs")))
    );

    // Test fuzzy match "cc" -> should find cache.rs (and maybe command.rs / completion.rs depending on score, but definitely cache.rs)
    let results_cc = super::get_file_completions_with_filter_sync(dir_path, "", Some("cc"));
    assert!(!results_cc.is_empty());
    assert!(
        results_cc
            .iter()
            .any(|c| matches!(c, Candidate::Item(name, _) if name.contains("cache.rs")))
    );

    // Test no match
    let results_none = super::get_file_completions_with_filter_sync(dir_path, "", Some("xyz"));
    assert!(results_none.is_empty());
}

#[test]
fn test_get_prompt_and_input_defaults() {
    let (prompt, input) = get_prompt_and_input_for_completion();
    assert_eq!(prompt, "$ ");
    assert_eq!(input, "");
}

#[test]
fn test_select_completion_items_empty_returns_none() {
    let result = select_completion_items(vec![], None, "$ ", "", CompletionFrameworkKind::Inline);
    assert!(matches!(result, CompletionSelection::None));
}

#[test]
fn test_select_completion_items_single_fast_path() {
    let items = vec![Candidate::Basic("hello".to_string())];
    let result = select_completion_items(items, None, "$ ", "h", CompletionFrameworkKind::Inline);
    // Single candidate → fast path returns Selected
    assert!(matches!(result, CompletionSelection::Selected(ref s) if s == "hello"));
}

#[test]
fn test_get_file_completions_nonexistent_dir() {
    let results = get_file_completions_with_filter_sync("/nonexistent/dir/12345", "", None);
    assert!(results.is_empty());
}

#[test]
fn test_get_file_completions_directories_labeled() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let dir_path = dir.path().to_str().unwrap();

    // Create a subdirectory
    std::fs::create_dir(dir.path().join("subdir")).unwrap();
    // Create a file
    std::fs::File::create(dir.path().join("file.txt")).unwrap();

    let results = get_file_completions_with_filter_sync(dir_path, "", None);
    assert_eq!(results.len(), 2);

    // Check labeling
    let has_dir_label = results
        .iter()
        .any(|c| matches!(c, Candidate::Item(_, desc) if desc == "(directory)"));
    let has_file_label = results
        .iter()
        .any(|c| matches!(c, Candidate::Item(_, desc) if desc == "(file)"));
    assert!(has_dir_label, "Directory should be labeled (directory)");
    assert!(has_file_label, "File should be labeled (file)");
}
