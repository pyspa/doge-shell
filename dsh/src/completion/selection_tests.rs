//! Legacy selection contracts and raw/preformatted insertion regressions.
use super::*;

#[test]
fn legacy_public_selection_changes_with_working_directory() {
    let _guard = crate::test_env_lock();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        struct RestoreCwd(std::path::PathBuf);
        impl Drop for RestoreCwd {
            fn drop(&mut self) {
                std::env::set_current_dir(&self.0).unwrap();
            }
        }
        let _restore = RestoreCwd(std::env::current_dir().unwrap());
        let root = tempfile::tempdir().unwrap();
        let mut shell = crate::shell::Shell::new(crate::environment::Environment::new_synthetic());
        let repl = Repl::new(&mut shell);
        let input = Input::new(crate::input::InputConfig::default());
        for name in ["apple", "banana"] {
            let dir = root.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            let filename = format!("legacy_public_scope_{name}");
            std::fs::write(dir.join(&filename), "").unwrap();
            std::env::set_current_dir(&dir).unwrap();
            let result = completion_from_current_with_prompt(
                &input,
                &repl,
                Some("legacy_public_scope_"),
                "",
                "",
                CompletionFrameworkKind::Inline,
            )
            .await;
            assert_eq!(result, CompletionSelection::Selected(filename));
        }
    });
}

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

fn legacy_test_snapshot(
    environment: &crate::environment::Environment,
    cwd: &Path,
) -> LegacyCompletionSnapshot {
    LegacyCompletionSnapshot::from_environment(environment, cwd.to_path_buf())
}

fn legacy_candidate_names(data: CurrentCompletionData) -> Vec<String> {
    data.items
        .into_iter()
        .map(|item| item.output().into_owned())
        .collect()
}

#[tokio::test]
async fn legacy_cache_follows_directory_without_changing_process_cwd() {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    std::fs::write(a.join("legacy_apple"), "").unwrap();
    std::fs::write(b.join("legacy_banana"), "").unwrap();
    let environment = crate::environment::Environment::new_synthetic();
    environment
        .write()
        .set_shell_var("PATH".into(), String::new());
    let cache = CompletionCache::new(Duration::from_secs(3));
    for (cwd, expected) in [
        (&a, "legacy_apple"),
        (&b, "legacy_banana"),
        (&a, "legacy_apple"),
    ] {
        let snapshot = legacy_test_snapshot(&environment.read(), cwd);
        let data = cached_current_context_candidates(&snapshot, "legacy_", &cache)
            .await
            .unwrap();
        assert_eq!(legacy_candidate_names(data), vec![expected]);
    }
}

#[tokio::test]
async fn legacy_cache_follows_logical_home_and_preserves_unset() {
    let root = tempfile::tempdir().unwrap();
    let environment = crate::environment::Environment::new_synthetic();
    let cache = CompletionCache::new(Duration::from_secs(3));
    for name in ["first", "second"] {
        let home = root.path().join(name);
        std::fs::create_dir(&home).unwrap();
        let filename = format!("legacy_{name}");
        std::fs::write(home.join(&filename), "").unwrap();
        environment
            .write()
            .set_shell_var("HOME".into(), home.display().to_string());
        let snapshot = legacy_test_snapshot(&environment.read(), root.path());
        let data = cached_current_context_candidates(&snapshot, "~/legacy_", &cache)
            .await
            .unwrap();
        assert_eq!(
            legacy_candidate_names(data),
            vec![home.join(filename).display().to_string()]
        );
    }
    let literal = root.path().join("~");
    std::fs::create_dir(&literal).unwrap();
    std::fs::write(literal.join("legacy_literal"), "").unwrap();
    environment.write().remove_shell_var("HOME");
    let snapshot = legacy_test_snapshot(&environment.read(), root.path());
    let data = cached_current_context_candidates(&snapshot, "~/legacy_", &cache)
        .await
        .unwrap();
    assert_eq!(legacy_candidate_names(data), vec!["~/legacy_literal"]);
}

#[tokio::test]
async fn legacy_cache_separates_paths_across_environments_with_equal_generations() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let cache = CompletionCache::new(Duration::from_secs(3));
    let mut generations = Vec::new();
    for name in ["first", "second"] {
        let bin = root.path().join(name);
        std::fs::create_dir(&bin).unwrap();
        let filename = format!("legacy_command_{name}");
        let executable = bin.join(&filename);
        std::fs::write(&executable, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let environment = crate::environment::Environment::new_synthetic();
        environment
            .write()
            .set_shell_var("PATH".into(), bin.display().to_string());
        environment
            .write()
            .set_shell_var("HOME".into(), root.path().display().to_string());
        let snapshot = legacy_test_snapshot(&environment.read(), root.path());
        generations.push(snapshot.path_generation);
        let data = cached_current_context_candidates(&snapshot, "legacy_command_", &cache)
            .await
            .unwrap();
        assert_eq!(legacy_candidate_names(data), vec![filename]);
        let previous_scope = snapshot.cache_scope();
        environment
            .write()
            .set_shell_var("PATH".into(), bin.display().to_string());
        assert_ne!(
            legacy_test_snapshot(&environment.read(), root.path()).cache_scope(),
            previous_scope
        );
    }
    assert_eq!(generations[0], generations[1]);
}

#[tokio::test]
async fn legacy_relative_and_empty_path_entries_use_the_snapshot_cwd() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    for executable in [root.path().join("legacy_local"), bin.join("legacy_bin")] {
        std::fs::write(&executable, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let environment = crate::environment::Environment::new_synthetic();
    environment
        .write()
        .set_shell_var("PATH".into(), ":bin".into());
    let snapshot = legacy_test_snapshot(&environment.read(), root.path());
    let data = collect_current_context_candidates(&snapshot, "legacy_")
        .await
        .unwrap();
    for name in ["legacy_local", "legacy_bin"] {
        assert!(data.items.iter().any(|candidate| {
            matches!(candidate, Candidate::Item(value, description) if value == name && description == "(command)")
        }));
    }
}

#[tokio::test]
async fn legacy_cache_hits_keep_the_initial_expiration() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("legacy_deadline"), "").unwrap();
    let environment = crate::environment::Environment::new_synthetic();
    environment
        .write()
        .set_shell_var("PATH".into(), String::new());
    let snapshot = legacy_test_snapshot(&environment.read(), root.path());
    let cache = CompletionCache::new(Duration::from_secs(3));
    cached_current_context_candidates(&snapshot, "legacy_", &cache)
        .await
        .unwrap();
    let expiration = cache
        .scoped_expiration(snapshot.cache_scope(), "legacy_")
        .unwrap();
    for _ in 0..3 {
        let data = cached_current_context_candidates(&snapshot, "legacy_", &cache)
            .await
            .unwrap();
        assert_eq!(legacy_candidate_names(data), vec!["legacy_deadline"]);
        assert_eq!(
            cache.scoped_expiration(snapshot.cache_scope(), "legacy_"),
            Some(expiration)
        );
    }
}

#[tokio::test]
async fn legacy_lisp_command_uses_the_logical_exported_environment() {
    use crate::{completion::AutoComplete, environment::Environment, shell::Shell};
    let environment = Environment::new_synthetic();
    environment
        .write()
        .set_and_export_shell_var("LEGACY_COMPLETION_VALUE".into(), "logical_value".into());
    let mut shell = Shell::new(environment);
    let repl = Repl::new(&mut shell);
    let callbacks = repl
        .shell
        .lisp_engine
        .borrow()
        .env
        .borrow()
        .autocompletion
        .clone();
    *callbacks.borrow_mut() = vec![AutoComplete {
        target: "legacy_probe".into(),
        cmd: Some("printf '%s' \"$LEGACY_COMPLETION_VALUE\"".into()),
        func: None,
        candidates: None,
    }];
    let mut input = Input::new(crate::input::InputConfig::default());
    input.reset("legacy_probe log".into());
    assert_eq!(
        input_completion(&input, &repl, Some("log"), Some("log"), "", input.as_str()).await,
        CompletionSelection::Selected("logical_value".into()),
    );
}

#[test]
fn cold_legacy_tab_keeps_files_and_refreshes_after_shared_scan() {
    let _guard = crate::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(dir.path().join("zz-cold-file"), "").unwrap();
    let env = crate::environment::Environment::new_synthetic();
    {
        let mut environment = env.write();
        environment.variable_state.paths = vec![bin.display().to_string()];
        environment.isolated_projection = false;
        environment.invocation.interactive = true;
    }
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker = env
        .read()
        .start_path_ghost_scan(move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(vec!["zz-cold-command".into()])
        })
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let filesystem = super::super::generators::filesystem::FileSystemContext::new(dir.path(), None);
    let ticket = super::super::generators::system::activate_system_command_cache_at(
        &env.read().variable_state.paths,
        &filesystem,
    );
    let mut snapshot = LegacyCompletionSnapshot::from_environment(&env.read(), dir.path().into());
    snapshot.system = Some((
        ticket,
        Arc::new(super::super::dynamic::CompletionRuntime::new()),
    ));
    let cache = CompletionCache::new(Duration::from_secs(60));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let first = runtime
        .block_on(cached_current_context_candidates(
            &snapshot, "zz-cold-", &cache,
        ))
        .unwrap();
    let second = runtime
        .block_on(cached_current_context_candidates(
            &snapshot, "zz-cold-", &cache,
        ))
        .unwrap();
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    for result in [first, second] {
        assert!(
            result
                .items
                .iter()
                .any(|item| item.output() == "zz-cold-file")
        );
        assert!(
            !result
                .items
                .iter()
                .any(|item| item.output() == "zz-cold-command")
        );
    }
    let ready = runtime
        .block_on(cached_current_context_candidates(
            &snapshot, "zz-cold-", &cache,
        ))
        .unwrap();
    assert!(
        ready
            .items
            .iter()
            .any(|item| item.output() == "zz-cold-command")
    );
    assert!(
        ready
            .items
            .iter()
            .any(|item| item.output() == "zz-cold-file")
    );
}
