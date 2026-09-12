use super::*;
use crate::test_support::TestShellProxy;
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;

#[test]
fn test_detect_package_json() {
    let dir = tempdir().unwrap();
    let package_json = r#"{
            "scripts": {
                "start": "node index.js",
                "test": "jest"
            }
        }"#;
    let mut file = File::create(dir.path().join("package.json")).unwrap();
    file.write_all(package_json.as_bytes()).unwrap();

    let tasks = list_tasks_in_dir(dir.path()).unwrap();
    let start_task = tasks.iter().find(|t| t.name == "start").unwrap();
    assert_eq!(start_task.source, "npm");
    assert_eq!(start_task.command, "npm run start");

    let test_task = tasks.iter().find(|t| t.name == "test").unwrap();
    assert_eq!(test_task.source, "npm");
}

#[test]
fn test_detect_cargo_toml() {
    let dir = tempdir().unwrap();
    File::create(dir.path().join("Cargo.toml")).unwrap();

    let tasks = list_tasks_in_dir(dir.path()).unwrap();
    assert!(
        tasks
            .iter()
            .any(|t| t.name == "build" && t.source == "cargo")
    );
    assert!(
        tasks
            .iter()
            .any(|t| t.name == "check" && t.source == "cargo")
    );
}

#[test]
fn parse_gradle_tasks_reads_task_names_from_gradle_output() {
    let output = r#"
Build tasks
-----------
assemble - Assembles the outputs of this project.
build - Assembles and tests this project.
:app:testDebugUnitTest - Run unit tests.
help
"#;

    assert_eq!(
        parse_gradle_task_names(output),
        vec![
            "assemble".to_string(),
            "build".to_string(),
            ":app:testDebugUnitTest".to_string(),
        ]
    );
}

#[test]
fn metadata_summary_does_not_execute_gradle() {
    let dir = tempdir().unwrap();
    File::create(dir.path().join("build.gradle")).unwrap();

    let summary = summarize_tasks_in_dir_metadata_only(dir.path()).unwrap();
    assert!(summary.tasks.is_empty());
    assert_eq!(summary.deferred_sources, vec!["gradle".to_string()]);
}

#[test]
fn test_detect_yarn() {
    let dir = tempdir().unwrap();
    let package_json = r#"{ "scripts": { "build": "echo build" } }"#;
    File::create(dir.path().join("package.json"))
        .unwrap()
        .write_all(package_json.as_bytes())
        .unwrap();
    File::create(dir.path().join("yarn.lock")).unwrap();

    let tasks = list_tasks_in_dir(dir.path()).unwrap();
    let task = tasks.first().unwrap();
    assert_eq!(task.source, "yarn");
    assert_eq!(task.command, "yarn run build");
}

#[test]
fn test_detect_tasks_from_project_root_when_called_from_subdir() {
    let dir = tempdir().unwrap();
    let package_json = r#"{ "scripts": { "build": "echo build" } }"#;
    File::create(dir.path().join("package.json"))
        .unwrap()
        .write_all(package_json.as_bytes())
        .unwrap();
    File::create(dir.path().join("mise.toml"))
        .unwrap()
        .write_all(b"[tasks.dev]\nrun = 'npm run dev'\n")
        .unwrap();
    let nested = dir.path().join("src").join("nested");
    fs::create_dir_all(&nested).unwrap();

    let tasks = list_tasks_in_dir(&nested).unwrap();
    assert!(
        tasks
            .iter()
            .any(|task| task.name == "build" && task.source == "npm")
    );
    assert!(
        tasks
            .iter()
            .any(|task| task.name == "dev" && task.source == "mise")
    );
}

#[test]
fn source_scoped_task_detection_does_not_execute_makefile_for_npm() {
    let dir = tempdir().unwrap();
    let marker = dir.path().join("should-not-exist");
    File::create(dir.path().join("package.json"))
        .unwrap()
        .write_all(br#"{ "scripts": { "build": "echo build" } }"#)
        .unwrap();
    File::create(dir.path().join("Makefile"))
        .unwrap()
        .write_all(format!("$(shell touch {})\nall:\n\t@echo all\n", marker.display()).as_bytes())
        .unwrap();

    let tasks = list_tasks_in_dir_for_sources(dir.path(), &["npm"]).unwrap();
    assert!(
        tasks
            .iter()
            .any(|task| task.source == "npm" && task.name == "build")
    );
    assert!(
        !marker.exists(),
        "npm-scoped task detection must not invoke make"
    );
}

#[test]
fn metadata_summary_does_not_execute_makefile() {
    let dir = tempdir().unwrap();
    let marker = dir.path().join("should-not-exist");
    File::create(dir.path().join("Makefile"))
        .unwrap()
        .write_all(format!("$(shell touch {})\nall:\n\t@echo all\n", marker.display()).as_bytes())
        .unwrap();

    let summary = summarize_tasks_in_dir_metadata_only(dir.path()).unwrap();
    assert!(summary.tasks.is_empty());
    assert_eq!(summary.deferred_sources, vec!["make".to_string()]);
    assert!(
        !marker.exists(),
        "metadata-only task summary must not invoke make"
    );
}

#[test]
fn parse_options_preserves_task_name_literals() {
    let args = vec![
        "--source".to_string(),
        "cargo".to_string(),
        "build".to_string(),
    ];
    let opts = parse_options(&args).unwrap();
    assert_eq!(opts.source.as_deref(), Some("cargo"));
    assert_eq!(opts.target.as_deref(), Some("build"));

    let args = vec!["npm:test".to_string()];
    let opts = parse_options(&args).unwrap();
    assert_eq!(opts.source.as_deref(), None);
    assert_eq!(opts.target.as_deref(), Some("npm:test"));
}

#[test]
fn parse_options_allows_colon_task_names_with_source_filter() {
    let args = vec![
        "--source".to_string(),
        "npm".to_string(),
        "lint:fix".to_string(),
    ];
    let opts = parse_options(&args).unwrap();
    assert_eq!(opts.source.as_deref(), Some("npm"));
    assert_eq!(opts.target.as_deref(), Some("lint:fix"));
}

#[test]
fn select_task_reports_ambiguous_names() {
    let tasks = vec![
        Task::test("cargo", "test", "cargo test"),
        Task::test("npm", "test", "npm run test"),
    ];

    match select_task(&tasks, None, "test") {
        TaskSelection::Ambiguous { matches, .. } => assert_eq!(matches.len(), 2),
        _ => panic!("expected ambiguous task selection"),
    }

    match select_task(&tasks, Some("cargo"), "test") {
        TaskSelection::Selected(task) => assert_eq!(task.command, "cargo test"),
        _ => panic!("expected source-qualified task selection"),
    }
}

#[test]
fn select_task_supports_qualified_source_without_breaking_colon_task_names() {
    let tasks = vec![
        Task::test("cargo", "build", "cargo build"),
        Task::test("npm", "lint:fix", "npm run lint:fix"),
    ];

    match select_task(&tasks, None, "cargo:build") {
        TaskSelection::Selected(task) => assert_eq!(task.command, "cargo build"),
        _ => panic!("expected known source-qualified task selection"),
    }

    match select_task(&tasks, None, "lint:fix") {
        TaskSelection::Selected(task) => assert_eq!(task.command, "npm run lint:fix"),
        _ => panic!("expected colon-containing task name to remain literal"),
    }

    match select_task(&tasks, Some("npm"), "lint:fix") {
        TaskSelection::Selected(task) => assert_eq!(task.command, "npm run lint:fix"),
        _ => panic!("expected source filter with colon-containing task name"),
    }
}

#[test]
fn filtered_tasks_supports_source_and_target_filters() {
    let tasks = vec![
        Task::test("cargo", "build", "cargo build"),
        Task::test("cargo", "test", "cargo test"),
        Task::test("npm", "test", "npm run test"),
    ];

    let filtered = filtered_tasks(&tasks, Some("cargo"), Some("test"));
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].command, "cargo test");
}

#[test]
fn filtered_tasks_for_request_supports_known_source_qualifier() {
    let tasks = vec![
        Task::test("cargo", "build", "cargo build"),
        Task::test("npm", "lint:fix", "npm run lint:fix"),
    ];

    let filtered = filtered_tasks_for_request(&tasks, None, Some("cargo:build"));
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].command, "cargo build");

    let filtered = filtered_tasks_for_request(&tasks, None, Some("lint:fix"));
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].command, "npm run lint:fix");
}

#[test]
fn command_dispatches_selected_task_through_shell_without_duplicate_sh() {
    let dir = tempdir().unwrap();
    File::create(dir.path().join("Cargo.toml")).unwrap();
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        allow_dispatch: true,
        ..TestShellProxy::default()
    };
    let pid = nix::unistd::getpid();
    let ctx = Context::new_safe(pid, pid, false);

    let status = command(
        &ctx,
        vec!["task".to_string(), "build".to_string()],
        &mut proxy,
    );

    assert_eq!(status, ExitStatus::ExitedWith(0));
    assert_eq!(
        proxy.dispatched,
        vec![(
            "sh".to_string(),
            vec![
                "-c".to_string(),
                format!(
                    "cd {} && cargo build",
                    dir.path().canonicalize().unwrap().display()
                )
            ]
        )]
    );
}

#[test]
fn forwards_arguments_after_separator_with_shell_quoting() {
    let task = Task::test("npm", "test", "npm run test");
    assert_eq!(
        task_execution_command(&task, &["--watch".to_string(), "two words".to_string()]),
        "cd /tmp && npm run test -- --watch 'two words'"
    );
    let options = parse_options(&[
        "npm:test".to_string(),
        "--".to_string(),
        "--watch".to_string(),
    ])
    .unwrap();
    assert_eq!(options.target.as_deref(), Some("npm:test"));
    assert_eq!(options.forward_args, vec!["--watch"]);

    let cargo = Task::test("cargo", "test", "cargo test");
    assert_eq!(
        task_execution_command(&cargo, &["--workspace".to_string()]),
        "cd /tmp && cargo test --workspace"
    );
}

#[test]
fn parses_machine_readable_mise_tasks() {
    let value = serde_json::json!([{"name": "test"}, {"name": "lint"}]);
    let tasks = parse_mise_tasks_json(&value);
    assert_eq!(tasks[0].name, "test");
    assert_eq!(tasks[0].command, "mise run test");
}

#[test]
fn parses_machine_readable_turbo_tasks_without_package_duplicates() {
    let value = serde_json::json!({
        "tasks": [
            {"taskId": "web#build", "task": "build", "package": "web"},
            {"taskId": "docs#build", "task": "build", "package": "docs"},
            {"taskId": "web#lint", "task": "lint", "package": "web"}
        ]
    });
    let tasks = parse_turbo_tasks_json(&value);
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].command, "turbo run build");
    assert_eq!(tasks[1].command, "turbo run lint");
}

#[test]
fn task_info_has_stable_qualified_id_and_cwd() {
    let task = TaskInfo::new("nx", "web:test", "nx run web:test", "/repo");
    assert_eq!(task.id, "nx:web:test");
    assert_eq!(task.cwd, "/repo");
}

#[test]
fn provider_process_is_killed_after_timeout() {
    let start = std::time::Instant::now();
    let result = command_output_with_timeout(
        Path::new("/bin/sh"),
        &["-c", "sleep 1"],
        Path::new("/tmp"),
        Duration::from_millis(20),
    );
    assert!(result.unwrap_err().to_string().contains("timed out"));
    assert!(start.elapsed() < Duration::from_millis(500));
}

#[test]
fn task_cache_invalidates_when_project_marker_changes() {
    let dir = tempdir().unwrap();
    let package = dir.path().join("package.json");
    fs::write(&package, r#"{"scripts":{"first":"echo first"}}"#).unwrap();
    let first = list_tasks_in_dir(dir.path()).unwrap();
    assert!(first.iter().any(|task| task.name == "first"));

    std::thread::sleep(Duration::from_millis(10));
    fs::write(&package, r#"{"scripts":{"second":"echo second"}}"#).unwrap();
    let second = list_tasks_in_dir(dir.path()).unwrap();
    assert!(second.iter().any(|task| task.name == "second"));
    assert!(!second.iter().any(|task| task.name == "first"));
}

#[test]
fn task_cache_invalidates_when_descendant_nx_marker_changes() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("workspace.json"), r#"{"projects":{}}"#).unwrap();
    let app = dir.path().join("apps/api");
    fs::create_dir_all(&app).unwrap();
    let project = app.join("project.json");
    fs::write(&project, r#"{"name":"api","targets":{"build":{}}}"#).unwrap();
    let first = list_tasks_in_dir(dir.path()).unwrap();
    assert!(first.iter().any(|task| task.name == "api:build"));

    std::thread::sleep(Duration::from_millis(10));
    fs::write(&project, r#"{"name":"api","targets":{"test":{}}}"#).unwrap();
    let second = list_tasks_in_dir(dir.path()).unwrap();
    assert!(second.iter().any(|task| task.name == "api:test"));
    assert!(!second.iter().any(|task| task.name == "api:build"));
}
