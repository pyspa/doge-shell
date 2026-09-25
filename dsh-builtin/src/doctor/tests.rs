use super::*;
use crate::chatgpt::skills::usage;
use crate::project_context;
use crate::test_support::TestShellProxy as TestProxy;
use dsh_types::mcp::{McpServerConfig, McpServerTrust, McpTransport};
use dsh_types::observed_output::{ObservedOutput, SharedOutputObserver};
use std::collections::HashMap;
use std::os::fd::IntoRawFd;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

fn observed_context() -> (Context, SharedOutputObserver) {
    let mut ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), false);
    let observer = ObservedOutput::shared(8192);
    ctx.output_observer = Some(observer.clone());
    ctx.outfile = std::fs::File::create("/dev/null").unwrap().into_raw_fd();
    ctx.errfile = std::fs::File::create("/dev/null").unwrap().into_raw_fd();
    (ctx, observer)
}

fn observed_stdout(observer: &SharedOutputObserver) -> String {
    observer.lock().unwrap().snapshot().stdout
}

#[test]
fn mask_secret_hides_prefix() {
    assert_eq!(mask_secret(Some("abcdef".to_string())), "***cdef");
    assert_eq!(mask_secret(None), "missing");
}

#[test]
fn show_section_matches_alias() {
    assert!(show_section(Some("runtime"), "runtimes"));
    assert!(show_section(None, "ai"));
    assert!(!show_section(Some("ai"), "mcp"));
}

#[test]
fn help_text_lists_sections_and_examples() {
    let help = help_text();
    assert!(help.contains("Usage: doctor"));
    assert!(help.contains("config"));
    assert!(help.contains("ai"));
    assert!(help.contains("mcp"));
    assert!(help.contains("project"));
    assert!(help.contains("runtime"));
    assert!(help.contains("performance"));
    assert!(help.contains("--latency"));
    assert!(help.contains("skills"));
    assert!(help.contains("safety"));
    assert!(help.contains("setup"));
    assert!(help.contains("fix"));
    assert!(help.contains("validate"));
    assert!(help.contains("doctor ai"));
    assert!(help.contains("doctor setup"));
}

#[test]
fn performance_latency_options_are_detected() {
    let args = vec![
        "--latency".to_string(),
        "--latency-iters".to_string(),
        "250".to_string(),
    ];
    assert!(performance_latency_enabled(&args));
    assert_eq!(performance_latency_iterations(&args), Some(250));
    assert!(!performance_latency_enabled(&[]));
    assert_eq!(performance_latency_iterations(&[]), None);
}

#[test]
fn performance_top_option_defaults_and_parses() {
    assert_eq!(performance_top_limit(&[]), PERFORMANCE_TOP_DEFAULT);
    assert_eq!(
        performance_top_limit(&["--top".to_string(), "3".to_string()]),
        3
    );
    assert_eq!(performance_top_limit(&["--top=7".to_string()]), 7);
    assert_eq!(
        performance_top_limit(&["--top".to_string(), "0".to_string()]),
        PERFORMANCE_TOP_DEFAULT
    );
    assert_eq!(
        performance_top_limit(&["--top".to_string(), "bad".to_string()]),
        PERFORMANCE_TOP_DEFAULT
    );
}

#[test]
fn latency_probe_summary_selects_slowest_focus() {
    let lines = vec![
        "latency completion_cache_lookup total=10us avg=10ns iterations=1".to_string(),
        "latency integrated_completion_git_subcommand_warm total=100us avg=100ns iterations=1"
            .to_string(),
        "latency repl_analyze_input total=50us avg=50ns iterations=1".to_string(),
    ];

    let (name, avg_ns) = slowest_latency_probe(&lines).expect("slowest probe");
    assert_eq!(name, "integrated_completion_git_subcommand_warm");
    assert_eq!(avg_ns, 100);
    assert_eq!(latency_probe_focus(name), "completion");
}

#[test]
fn show_section_matches_new_aliases() {
    assert!(show_section(Some("validate"), "dev"));
    assert!(is_known_section("skills"));
    assert!(is_known_section("safety"));
    assert!(is_known_section("setup"));
    assert!(is_known_section("fix"));
    assert!(!is_known_section("unknown"));
}

#[test]
fn default_config_lisp_contains_safe_setup_defaults() {
    let config = default_config_lisp();
    assert!(config.contains("chat-execute-clear"));
    assert!(config.contains("chat-execute-add"));
    assert!(config.contains("allow-direnv"));
}

#[test]
fn project_section_uses_resolved_context() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("mise.toml"), "[tools]\nnode = '20.11.0'\n").unwrap();
    std::fs::write(dir.path().join("package.json"), "{\"name\":\"demo\"}").unwrap();

    let project = project_context::resolve_project_context(dir.path());
    let expected_root = std::fs::canonicalize(dir.path()).unwrap();
    let actual_root = std::fs::canonicalize(&project.project_root).unwrap();
    assert_eq!(actual_root, expected_root);
    assert!(
        project
            .project_markers
            .iter()
            .any(|marker| marker == "mise.toml")
    );
    assert!(
        project
            .runtimes
            .iter()
            .any(|runtime| runtime.name == "node" && runtime.source == "mise")
    );
}

#[test]
fn count_skill_dirs_only_counts_skill_folders() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("doge-shell-repo");
    let plain_dir = dir.path().join("notes");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::create_dir_all(&plain_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "# skill").unwrap();
    std::fs::write(plain_dir.join("README.md"), "# note").unwrap();

    assert_eq!(count_skill_dirs(dir.path()), 1);
}

#[test]
fn skills_text_and_json_use_the_same_codex_runtime_root() {
    let codex_home = tempfile::tempdir().unwrap();
    let expected = codex_home.path().join("skills");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let mut proxy = TestProxy {
        current_dir: repo_root.clone(),
        vars: HashMap::from([(
            "CODEX_HOME".to_string(),
            codex_home.path().display().to_string(),
        )]),
        ..TestProxy::default()
    };

    let details = json_section_details(&mut proxy, &repo_root, Some("skills"));
    assert_eq!(
        details["codex_runtime"]["path"],
        serde_json::Value::String(expected.display().to_string())
    );

    let (ctx, observer) = observed_context();
    check_skills(&ctx, &mut proxy, &repo_root);
    let output = observed_stdout(&observer);
    assert!(
        output.contains(&format!("root={}", expected.display())),
        "{output}"
    );
    assert!(!output.contains("skills/skills"), "{output}");
}

#[test]
fn skill_dirs_match_detects_stale_runtime_copy() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let dest = dir.path().join("dest");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(source.join("SKILL.md"), "# skill\n").unwrap();
    std::fs::write(dest.join("SKILL.md"), "# skill\n").unwrap();

    assert!(skill_dirs_match(&source, &dest));

    std::fs::write(dest.join("SKILL.md"), "# stale\n").unwrap();
    assert!(!skill_dirs_match(&source, &dest));
}

#[test]
fn parse_git_status_short_handles_renames() {
    let paths = parse_git_status_short(
        " M dsh-builtin/src/task.rs\nR  old/path.rs -> dsh/src/new_path.rs\n?? docs/ai/new.md\n",
    );
    assert_eq!(paths[0], PathBuf::from("dsh-builtin/src/task.rs"));
    assert_eq!(paths[1], PathBuf::from("dsh/src/new_path.rs"));
    assert_eq!(paths[2], PathBuf::from("docs/ai/new.md"));
}

#[test]
fn validation_commands_follow_changed_paths() {
    let paths = vec![
        PathBuf::from("dsh-builtin/src/task.rs"),
        PathBuf::from("dsh/src/lib.rs"),
        PathBuf::from("docs/ai/README.md"),
    ];
    let commands = validation_commands_for_paths(&paths);

    assert!(commands.iter().any(|cmd| cmd == "cargo fmt --check"));
    assert!(
        commands
            .iter()
            .any(|cmd| cmd == "cargo test -p dsh-builtin")
    );
    assert!(commands.iter().any(|cmd| cmd == "cargo test -p doge-shell"));
    assert!(commands.iter().any(|cmd| cmd == "cargo check --workspace"));
    assert!(
        commands
            .iter()
            .any(|cmd| cmd == "scripts/check-ai-guidance.sh")
    );
    assert!(commands.iter().any(|cmd| {
            cmd == "scripts/install-runtime-skills.sh --check-installed --target codex --profile codex-core"
        }));
    assert!(
        commands
            .iter()
            .any(|cmd| cmd == "scripts/check-portability.py"),
        "a Rust edit must propose the portability lint: {commands:?}"
    );
}

/// The linker flags and the CI matrix are the two non-Rust inputs the
/// portability lint covers, so they have to reach it without a `.rs` change.
#[test]
fn build_and_ci_paths_request_the_portability_check() {
    for path in [".cargo/config.toml", ".github/workflows/ci.yml"] {
        let commands = validation_commands_for_paths(&[PathBuf::from(path)]);
        assert!(
            commands
                .iter()
                .any(|cmd| cmd == "scripts/check-portability.py"),
            "{path} must propose the portability lint: {commands:?}"
        );
    }
}

#[test]
fn manifests_and_license_docs_request_the_consistency_check() {
    for path in ["Cargo.toml", "dsh/Cargo.toml", "README.md", "LICENSE"] {
        let commands = validation_commands_for_paths(&[PathBuf::from(path)]);
        assert!(
            commands
                .iter()
                .any(|cmd| cmd == "scripts/check-project-consistency.py"),
            "{path} must propose the project consistency check: {commands:?}"
        );
    }
}

/// Editing a completion definition is not a portability question, and the
/// suggestion list is only useful while it stays short.
#[test]
fn completion_definitions_do_not_request_the_portability_check() {
    let commands = validation_commands_for_paths(&[PathBuf::from("completions/git.json")]);
    assert!(
        !commands
            .iter()
            .any(|cmd| cmd == "scripts/check-portability.py"),
        "{commands:?}"
    );
}

#[test]
fn completion_definitions_map_to_the_embedding_package() {
    let commands = validation_commands_for_paths(&[PathBuf::from("completions/git.json")]);
    assert!(
        commands.iter().any(|cmd| cmd == "cargo test -p doge-shell"),
        "editing completions/ must still propose the doge-shell tests: {commands:?}"
    );

    let commands =
        validation_commands_for_paths(&[PathBuf::from("command-completion-schema.json")]);
    assert!(commands.iter().any(|cmd| cmd == "cargo test -p doge-shell"));
    assert!(commands.iter().any(|cmd| cmd == "cargo test -p dsh-types"));
}

/// `output-schemas/*.json` is embedded the same way `completions/` is,
/// and `command-output-schema.json` mirrors `command-completion-schema.json`.
#[test]
fn output_schemas_map_to_the_embedding_package() {
    let commands = validation_commands_for_paths(&[PathBuf::from("output-schemas/ps.json")]);
    assert!(commands.iter().any(|cmd| cmd == "cargo test -p doge-shell"));
    assert!(
        !commands
            .iter()
            .any(|cmd| cmd == "scripts/check-portability.py"),
        "{commands:?}"
    );

    let commands = validation_commands_for_paths(&[PathBuf::from("command-output-schema.json")]);
    assert!(commands.iter().any(|cmd| cmd == "cargo test -p doge-shell"));
    assert!(commands.iter().any(|cmd| cmd == "cargo test -p dsh-types"));
}

#[test]
fn claude_guidance_paths_request_the_guidance_check() {
    let commands = validation_commands_for_paths(&[PathBuf::from(".claude/settings.json")]);
    assert!(
        commands
            .iter()
            .any(|cmd| cmd == "scripts/check-ai-guidance.sh"),
        "{commands:?}"
    );
}

#[test]
fn touching_the_proxy_facade_requests_the_capability_check() {
    for path in [
        "dsh-builtin/src/lib.rs",
        "dsh-builtin/src/shell_capabilities.rs",
        "dsh-builtin/src/capability.rs",
    ] {
        let commands = validation_commands_for_paths(&[PathBuf::from(path)]);
        assert!(
            commands
                .iter()
                .any(|cmd| cmd == "scripts/check-shell-proxy-capabilities.py"),
            "{path}: {commands:?}"
        );
    }

    let unrelated = validation_commands_for_paths(&[PathBuf::from("dsh/src/repl/mod.rs")]);
    assert!(
        !unrelated
            .iter()
            .any(|cmd| cmd == "scripts/check-shell-proxy-capabilities.py"),
        "{unrelated:?}"
    );
}

#[test]
fn rust_and_guidance_changes_request_the_file_budget_check() {
    for path in [
        "dsh/src/repl/mod.rs",
        "dsh-builtin/src/task.rs",
        "docs/ai/skills/doge-shell-repo/references/task-map.md",
        "AGENTS.md",
        "CLAUDE.md",
    ] {
        let commands = validation_commands_for_paths(&[PathBuf::from(path)]);
        assert!(
            commands
                .iter()
                .any(|cmd| cmd == "scripts/check-file-budget.py"),
            "{path}: {commands:?}"
        );
    }
}

#[test]
fn completion_definitions_do_not_request_the_file_budget_check() {
    // The suggestion list stays short for the most frequent task; the
    // touch reminder below (notes, not commands) covers the real risk.
    for path in ["completions/git.json", "output-schemas/ps.json"] {
        let commands = validation_commands_for_paths(&[PathBuf::from(path)]);
        assert!(
            !commands
                .iter()
                .any(|cmd| cmd == "scripts/check-file-budget.py"),
            "{path}: {commands:?}"
        );
    }
}

#[test]
fn embedded_json_changes_note_the_loader_touch() {
    for path in ["completions/git.json", "output-schemas/ps.json"] {
        let notes = notes_for_paths(&[PathBuf::from(path)]);
        assert!(
            notes.iter().any(|note| note.contains("touch")),
            "{path}: {notes:?}"
        );
    }

    let unrelated = notes_for_paths(&[PathBuf::from("dsh/src/repl/mod.rs")]);
    assert!(unrelated.is_empty(), "{unrelated:?}");
}

#[test]
fn safety_helpers_flag_risky_allowlist_entries() {
    assert!(is_risky_execute_allowlist_entry("bash"));
    assert!(is_risky_execute_allowlist_entry("python -c"));
    assert!(is_risky_execute_allowlist_entry("rm -rf /tmp/demo"));
    assert!(!is_risky_execute_allowlist_entry("git status"));
    assert!(is_sensitive_env_name("API_TOKEN"));
    assert!(is_sensitive_env_name("PRIVATE_KEY"));
    assert!(is_sensitive_env_name("GOOGLE_APPLICATION_CREDENTIALS"));
    assert!(!is_sensitive_env_name("PATH"));
}

#[test]
fn safety_url_helper_rejects_localhost_prefix_spoofing() {
    assert!(is_https_or_local_http_url("https://api.example.com/v1"));
    assert!(is_https_or_local_http_url("http://localhost:8080/v1"));
    assert!(is_https_or_local_http_url("http://127.0.0.1/v1"));
    assert!(is_https_or_local_http_url("http://[::1]:8080/v1"));
    assert!(!is_https_or_local_http_url("http://example.com/v1"));
    assert!(!is_https_or_local_http_url("http://localhost.evil.com/v1"));
    assert!(!is_https_or_local_http_url("http://127.0.0.1.evil.com/v1"));
}

/// One line per root, named after the directory rather than the scope: a
/// project has two roots, and one `project-skills` line for both would
/// report the shared directory's entry count as the repository's.
#[test]
fn doctor_skills_labels_each_root_separately() {
    let project = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    for (dir, name) in [(".dogesh/skills", "deploy"), (".agents/skills", "review")] {
        let skill = root.join(dir).join(name);
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: d\n---\n"),
        )
        .unwrap();
    }

    let (ctx, observer) = observed_context();
    let mut proxy = hooks_proxy(&root, &[]);
    crate::chatgpt::skills::clear_skills_fragment_cache();
    report_runtime_skills(&ctx, &mut proxy, &root);
    let output = observed_stdout(&observer);

    assert!(output.contains("ok project-skills "), "{output}");
    assert!(output.contains("ok project-agents-skills "), "{output}");
    // Both are untrusted here, and each gets its own line.
    assert_eq!(
        output.matches("project-skills-trust").count(),
        2,
        "{output}"
    );
}

/// The deep lint pass runs from `doctor skills`, not from the load path,
/// so a skill missing its `description` shows up as an `error` line in
/// addition to the existing load-time `SkillDiagnostic` warning.
#[test]
fn doctor_skills_reports_a_lint_rejection_as_an_error() {
    let project = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let skill = root.join(".dogesh/skills/broken");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "---\nname: broken\n---\n\nbody\n").unwrap();

    let (ctx, observer) = observed_context();
    let mut proxy = hooks_proxy(&root, &[]);
    crate::chatgpt::skills::clear_skills_fragment_cache();
    report_runtime_skills(&ctx, &mut proxy, &root);
    let output = observed_stdout(&observer);

    assert!(
        output.contains("error project-skill") && output.contains("description"),
        "{output}"
    );
}

/// Archived and pending are their own lines, and an archived skill is
/// counted there instead of under `unused-skills` - reported twice would
/// read as two different problems for one decision.
#[test]
fn doctor_skills_reports_archived_and_pending_separately() {
    let _lock = crate::chatgpt::tool::execute::tests::env_lock();
    let project = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let skill = root.join(".dogesh/skills/demo");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: d\n---\n\nbody\n",
    )
    .unwrap();

    let state = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("XDG_STATE_HOME");
    // SAFETY: single-threaded under `env_lock`.
    unsafe { std::env::set_var("XDG_STATE_HOME", state.path()) };

    usage::set_archived(&skill, true).unwrap();
    crate::chatgpt::skills::pending::stage(crate::chatgpt::skills::pending::Proposal {
        version: 0,
        id: "project.other".to_string(),
        scope: "project".to_string(),
        name: "other".to_string(),
        file: "SKILL.md".to_string(),
        action: "create".to_string(),
        project_root: Some(root.join(".dogesh/skills")),
        contents: "---\nname: other\ndescription: d\n---\n".to_string(),
        base_digest: None,
        created_ms: usage::now_ms(),
        origin: "tool".to_string(),
        note: None,
    })
    .unwrap();

    let (ctx, observer) = observed_context();
    let mut proxy = hooks_proxy(&root, &[]);
    crate::chatgpt::skills::clear_skills_fragment_cache();
    report_runtime_skills(&ctx, &mut proxy, &root);
    let output = observed_stdout(&observer);

    match previous {
        Some(value) => unsafe { std::env::set_var("XDG_STATE_HOME", value) },
        None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
    }

    assert!(output.contains("ok archived-skills 1"), "{output}");
    assert!(output.contains("ok unused-skills 0"), "{output}");
    assert!(
        output.contains("warn pending-skills 1 awaiting review: other"),
        "{output}"
    );
}

fn hooks_proxy(cwd: &Path, vars: &[(&str, &str)]) -> TestProxy {
    TestProxy {
        current_dir: cwd.to_path_buf(),
        vars: vars
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        ..TestProxy::default()
    }
}

fn run_doctor_hooks(proxy: &mut TestProxy) -> String {
    let (ctx, observer) = observed_context();
    let status = command(&ctx, vec!["doctor".to_string(), "hooks".to_string()], proxy);
    assert_eq!(status, ExitStatus::ExitedWith(0));
    observed_stdout(&observer)
}

#[test]
fn doctor_hooks_reports_a_missing_config_as_skip() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("ai-hooks.json");
    let mut proxy = hooks_proxy(
        dir.path(),
        &[("DOGESH_AI_HOOKS_CONFIG", missing.to_str().unwrap())],
    );

    let output = run_doctor_hooks(&mut proxy);

    assert!(output.contains("[hooks]"), "{output}");
    assert!(output.contains("skip config none"), "{output}");
}

#[test]
fn doctor_hooks_flags_an_unresolvable_command() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("ai-hooks.json");
    std::fs::write(
            &config,
            r#"{"version":1,"hooks":[{"id":"gone","events":["pre-tool-use"],"command":["/definitely/not/here"]}]}"#,
        )
        .unwrap();
    let mut proxy = hooks_proxy(
        dir.path(),
        &[("DOGESH_AI_HOOKS_CONFIG", config.to_str().unwrap())],
    );

    let output = run_doctor_hooks(&mut proxy);

    assert!(output.contains("ok hook gone"), "{output}");
    assert!(
        output.contains("warn hook gone command not found"),
        "{output}"
    );
}

/// A hook narrowed by `programs` or `paths` should say so: "tools=execute"
/// alone reads as "every command", which is what it used to mean.
#[test]
fn doctor_hooks_shows_every_match_kind() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("ai-hooks.json");
    std::fs::write(
        &config,
        r#"{"version":1,"hooks":[{"id":"narrow","events":["pre-tool-use"],
               "match":{"tools":["execute"],"programs":["rm"],"paths":["/etc/**"],
                        "arguments":{"cwd":"/srv"}},
               "command":["sh"]}]}"#,
    )
    .unwrap();
    let mut proxy = hooks_proxy(
        dir.path(),
        &[("DOGESH_AI_HOOKS_CONFIG", config.to_str().unwrap())],
    );

    let output = run_doctor_hooks(&mut proxy);

    assert!(output.contains("tools=execute"), "{output}");
    assert!(output.contains("programs=rm"), "{output}");
    assert!(output.contains("paths=/etc/**"), "{output}");
    assert!(output.contains("args=cwd"), "{output}");
}

/// Reported, not refused: the configuration works, but the author needs to
/// know their matcher can never be true on one of the events they listed.
#[test]
fn doctor_hooks_warns_about_a_match_that_cannot_be_satisfied() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("ai-hooks.json");
    std::fs::write(
        &config,
        r#"{"version":1,"hooks":[{"id":"wide","events":["pre-tool-use","session-start"],
               "match":{"tools":["execute"],"programs":["rm"]},"command":["sh"]}]}"#,
    )
    .unwrap();
    let mut proxy = hooks_proxy(
        dir.path(),
        &[("DOGESH_AI_HOOKS_CONFIG", config.to_str().unwrap())],
    );

    let output = run_doctor_hooks(&mut proxy);

    assert!(
        output.contains("cannot be satisfied on session-start"),
        "{output}"
    );
    assert!(
        !output.contains("cannot be satisfied on pre-tool-use"),
        "{output}"
    );
}

#[test]
fn doctor_hooks_json_reports_the_turn_budget_and_the_matcher() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("ai-hooks.json");
    std::fs::write(
        &config,
        r#"{"version":1,"hooks":[{"id":"narrow","events":["pre-tool-use"],
               "match":{"tools":["execute"],"programs":["rm"]},"command":["sh"]}]}"#,
    )
    .unwrap();
    let mut proxy = hooks_proxy(
        dir.path(),
        &[
            ("DOGESH_AI_HOOKS_CONFIG", config.to_str().unwrap()),
            ("AI_CHAT_HOOK_TURN_BUDGET_MS", "2500"),
        ],
    );

    let details = json_hooks_details(&mut proxy);

    assert_eq!(details["turn_budget_ms"], 2500);
    assert_eq!(details["hooks"][0]["match"]["programs"][0], "rm");
}

/// A report that lists hooks while none of them can fire reads as "these
/// are running".
#[test]
fn doctor_hooks_says_nothing_runs_when_the_switch_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("ai-hooks.json");
    std::fs::write(
        &config,
        r#"{"version":1,"hooks":[{"id":"guard","events":["pre-tool-use"],"command":["/bin/sh"]}]}"#,
    )
    .unwrap();
    let mut proxy = hooks_proxy(
        dir.path(),
        &[
            ("DOGESH_AI_HOOKS_CONFIG", config.to_str().unwrap()),
            ("AI_CHAT_HOOKS", "off"),
        ],
    );

    let output = run_doctor_hooks(&mut proxy);

    assert!(
        output.contains("skip enabled AI_CHAT_HOOKS=off"),
        "{output}"
    );
    assert!(!output.contains("hook guard"), "{output}");
}

#[test]
fn doctor_hooks_flags_a_world_writable_config() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("ai-hooks.json");
    std::fs::write(&config, r#"{"version":1,"hooks":[]}"#).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o666)).unwrap();
    let mut proxy = hooks_proxy(
        dir.path(),
        &[("DOGESH_AI_HOOKS_CONFIG", config.to_str().unwrap())],
    );

    let output = run_doctor_hooks(&mut proxy);

    assert!(output.contains("error config"), "{output}");
    assert!(output.contains("world-writable"), "{output}");
}

#[test]
fn doctor_safety_reports_risky_posture() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("docs/ai")).unwrap();
    std::fs::write(dir.path().join(".gitignore"), "!debug.log\n").unwrap();
    std::fs::write(dir.path().join(".envrc"), "export FOO=bar\n").unwrap();
    std::fs::write(dir.path().join("debug.log"), "debug\n").unwrap();
    let git_init = StdCommand::new("git")
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(git_init.status.success());

    let mut mcp_env = HashMap::new();
    mcp_env.insert("API_TOKEN".to_string(), "secret".to_string());
    let mut vars = HashMap::new();
    vars.insert(
        "AI_CHAT_BASE_URL".to_string(),
        "http://example.com/v1".to_string(),
    );
    let mut proxy = TestProxy {
        current_dir: dir.path().to_path_buf(),
        vars,
        execute_allowlist: vec!["bash".to_string(), "git status".to_string()],
        // The safety `git status` resolves through the proxy's logical
        // PATH: point it at the runner's own PATH so the real `git` above
        // is found.
        command_search_paths: std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .collect(),
        mcp_servers: vec![
            McpServerConfig {
                label: "local".to_string(),
                description: None,
                trust: McpServerTrust::Untrusted,
                transport: McpTransport::Stdio {
                    command: "node".to_string(),
                    args: Vec::new(),
                    env: mcp_env,
                    cwd: None,
                },
            },
            McpServerConfig {
                label: "legacy".to_string(),
                description: None,
                trust: McpServerTrust::Untrusted,
                transport: McpTransport::Sse {
                    url: "https://example.com/sse".to_string(),
                },
            },
        ],
        ..TestProxy::default()
    };
    let (ctx, observer) = observed_context();

    let status = command(
        &ctx,
        vec!["doctor".to_string(), "safety".to_string()],
        &mut proxy,
    );

    assert_eq!(status, ExitStatus::ExitedWith(0));
    let output = observed_stdout(&observer);
    assert!(output.contains("[safety]"));
    assert!(output.contains("warn execute-allowlist risky `bash`"));
    assert!(output.contains("ok execute-allowlist `git status`"));
    assert!(output.contains("warn mcp local trust=untrusted stdio command=node sensitive-env"));
    assert!(output.contains(
        "warn mcp legacy trust=untrusted sse url=https://example.com/sse configuration-only use-streamable-http"
    ));
    assert!(output.contains("warn ai-base-url insecure http://example.com/v1"));
    assert!(output.contains("warn envrc not-allowed"));
    assert!(output.contains("warn unignored-log debug.log"));

    let (ctx, observer) = observed_context();
    let status = command(
        &ctx,
        vec![
            "doctor".to_string(),
            "safety".to_string(),
            "--json".to_string(),
        ],
        &mut proxy,
    );
    assert_eq!(status, ExitStatus::ExitedWith(0));
    let value: serde_json::Value = serde_json::from_str(observed_stdout(&observer).trim()).unwrap();
    assert_eq!(value["section"], "safety");
    assert_eq!(value["details"]["execute_allowlist"][0]["risky"], true);
    assert_eq!(value["details"]["mcp"]["sse_servers"], 1);
    assert_eq!(value["details"]["envrc"]["allowed"], false);
}

/// A trusted server is not a misconfiguration, but it relaxes the Normal
/// policy - so `doctor safety` reports it as a `warn` posture line.
#[test]
fn doctor_safety_reports_mcp_trust() {
    let mut proxy = TestProxy {
        current_dir: PathBuf::from("."),
        mcp_servers: vec![
            McpServerConfig {
                label: "plain".to_string(),
                description: None,
                trust: McpServerTrust::Untrusted,
                transport: McpTransport::Http {
                    url: "https://example.com/mcp".to_string(),
                    auth_header: None,
                    allow_stateless: None,
                },
            },
            McpServerConfig {
                label: "internal".to_string(),
                description: None,
                trust: McpServerTrust::Trusted,
                transport: McpTransport::Http {
                    url: "https://example.com/mcp".to_string(),
                    auth_header: None,
                    allow_stateless: None,
                },
            },
        ],
        ..TestProxy::default()
    };
    let (ctx, observer) = observed_context();

    let status = command(
        &ctx,
        vec!["doctor".to_string(), "safety".to_string()],
        &mut proxy,
    );

    assert_eq!(status, ExitStatus::ExitedWith(0));
    let output = observed_stdout(&observer);
    assert!(output.contains("ok mcp plain trust=untrusted http url=https://example.com/mcp"));
    assert!(output.contains("warn mcp internal trust=trusted http url=https://example.com/mcp"));
}

#[test]
fn validate_json_contains_focused_commands() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("docs/ai")).unwrap();
    std::fs::create_dir_all(dir.path().join("dsh/src")).unwrap();
    assert!(
        StdCommand::new("git")
            .arg("init")
            .current_dir(dir.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    std::fs::write(dir.path().join("dsh/src/review.rs"), "// changed\n").unwrap();

    // The dev `git status` resolves through the proxy's logical PATH: point
    // it at the runner's own PATH so the real `git` above is found.
    let mut proxy = TestProxy {
        current_dir: dir.path().to_path_buf(),
        command_search_paths: std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .collect(),
        ..TestProxy::default()
    };
    let value = json_dev_details(&mut proxy, dir.path());
    let commands = value["commands"].as_array().unwrap();
    assert!(
        commands
            .iter()
            .any(|command| command == "cargo test -p doge-shell")
    );
    assert!(
        value["changed_files"]
            .as_array()
            .is_some_and(|files| !files.is_empty())
    );
}

#[test]
fn doctor_mcp_reports_legacy_sse_as_configuration_only() {
    let mut proxy = TestProxy {
        current_dir: PathBuf::from("."),
        mcp_servers: vec![McpServerConfig {
            label: "legacy".to_string(),
            description: None,
            trust: McpServerTrust::Untrusted,
            transport: McpTransport::Sse {
                url: "https://example.com/sse".to_string(),
            },
        }],
        ..TestProxy::default()
    };
    let (ctx, observer) = observed_context();

    let status = command(
        &ctx,
        vec!["doctor".to_string(), "mcp".to_string()],
        &mut proxy,
    );

    assert_eq!(status, ExitStatus::ExitedWith(0));
    let output = observed_stdout(&observer);
    assert!(output.contains("[mcp]"));
    assert!(output.contains("ok configured 1"));
    assert!(output.contains(
        "warn mcp legacy sse url=https://example.com/sse configuration-only use-streamable-http"
    ));
}

#[test]
fn doctor_mcp_warns_on_large_tool_footprint() {
    let mut proxy = TestProxy {
        current_dir: PathBuf::from("."),
        vars: HashMap::from([("MCP_TOOLS".to_string(), "42".to_string())]),
        ..TestProxy::default()
    };
    let (ctx, observer) = observed_context();

    let status = command(
        &ctx,
        vec!["doctor".to_string(), "mcp".to_string()],
        &mut proxy,
    );

    assert_eq!(status, ExitStatus::ExitedWith(0));
    let output = observed_stdout(&observer);
    assert!(output.contains("ok tools 42"), "{output}");
    assert!(
        output.contains("warn mcp-tools-footprint high tools=42"),
        "{output}"
    );
}

#[test]
fn doctor_mcp_reports_small_tool_footprint_as_ok() {
    let mut proxy = TestProxy {
        current_dir: PathBuf::from("."),
        vars: HashMap::from([("MCP_TOOLS".to_string(), "3".to_string())]),
        ..TestProxy::default()
    };
    let (ctx, observer) = observed_context();

    let status = command(
        &ctx,
        vec!["doctor".to_string(), "mcp".to_string()],
        &mut proxy,
    );

    assert_eq!(status, ExitStatus::ExitedWith(0));
    let output = observed_stdout(&observer);
    assert!(
        output.contains("ok mcp-tools-footprint tools=3"),
        "{output}"
    );
}

/// Disabling groups quiets the footprint warning: it is driven by the active
/// count, while the registered total is still reported.
#[test]
fn doctor_mcp_footprint_follows_active_tools() {
    let mut proxy = TestProxy {
        current_dir: PathBuf::from("."),
        vars: HashMap::from([
            ("MCP_TOOLS".to_string(), "42".to_string()),
            ("MCP_ACTIVE_TOOLS".to_string(), "5".to_string()),
            ("MCP_ACTIVE_GROUPS".to_string(), "2".to_string()),
        ]),
        ..TestProxy::default()
    };
    let (ctx, observer) = observed_context();

    let status = command(
        &ctx,
        vec!["doctor".to_string(), "mcp".to_string()],
        &mut proxy,
    );

    assert_eq!(status, ExitStatus::ExitedWith(0));
    let output = observed_stdout(&observer);
    assert!(output.contains("ok tools 42"), "{output}");
    assert!(output.contains("ok active-tools 5"), "{output}");
    assert!(output.contains("ok active-groups 2"), "{output}");
    assert!(
        output.contains("ok mcp-tools-footprint tools=5"),
        "{output}"
    );
}

#[cfg(unix)]
mod runtime_authority_tests {
    use super::*;

    fn write_executable(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    /// Proxy whose logical PATH holds the fake tool, without touching the
    /// process-global PATH.
    fn logical_proxy(dir: &Path) -> TestProxy {
        TestProxy {
            current_dir: dir.to_path_buf(),
            command_search_paths: vec![dir.to_path_buf()],
            ..TestProxy::default()
        }
    }

    /// Text and JSON `runtime` reports resolve the same executable through
    /// the logical runtime, and the version probe runs that absolute path.
    #[test]
    fn text_and_json_runtime_reports_agree_on_logical_executable() {
        let dir = tempfile::tempdir().unwrap();
        write_executable(
            dir.path(),
            "cargo",
            "#!/bin/sh\necho 'cargo 9.9.9-logical'\n",
        );
        let mut proxy = logical_proxy(dir.path());

        let (ctx, observer) = observed_context();
        check_runtimes(&ctx, &mut proxy);
        let text = observed_stdout(&observer);
        assert!(
            text.contains(&format!(
                "ok cargo cargo 9.9.9-logical {}",
                dir.path().join("cargo").display()
            )),
            "text report must name the logical executable: {text}"
        );

        let details = json_section_details(&mut proxy, dir.path(), Some("runtime"));
        let commands = details["commands"].as_array().unwrap();
        let cargo = commands
            .iter()
            .find(|entry| entry["command"] == "cargo")
            .expect("cargo entry present");
        assert_eq!(
            cargo["path"].as_str().unwrap(),
            dir.path().join("cargo").to_string_lossy().as_ref()
        );
        assert_eq!(cargo["version"].as_str().unwrap(), "cargo 9.9.9-logical");
    }

    /// A tool on the process PATH only is reported as not-found: doctor
    /// never falls back to the process-global environment. The decoy goes
    /// in front of the runner's own PATH so parallel tests spawning bare
    /// system tools keep resolving.
    #[test]
    fn process_only_tool_is_not_found() {
        let _lock = crate::chatgpt::tool::execute::tests::env_lock();
        let process = tempfile::tempdir().unwrap();
        write_executable(process.path(), "cargo", "#!/bin/sh\necho 'cargo 1.0.0'\n");
        let previous = std::env::var_os("PATH");
        let mut entries = vec![process.path().into()];
        if let Some(previous) = &previous {
            entries.extend(std::env::split_paths(previous));
        }
        unsafe { std::env::set_var("PATH", std::env::join_paths(entries).unwrap()) };
        let empty = tempfile::tempdir().unwrap();
        let mut proxy = logical_proxy(empty.path());
        let (ctx, observer) = observed_context();
        check_runtimes(&ctx, &mut proxy);
        let result = observed_stdout(&observer);
        unsafe {
            match previous {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }
        assert!(
            result.contains("warn cargo not-found"),
            "process-only cargo must be invisible: {result}"
        );
    }

    /// `DOGESH_HERDR_ENABLED` is a logical shell setting: `unset` in the
    /// shell disables the pane report even when the process environment
    /// still carries a stale truthy value. The `HERDR_*` process identity
    /// stays process-global.
    #[test]
    fn herdr_enabled_reads_the_logical_shell_variable() {
        let _lock = crate::chatgpt::tool::execute::tests::env_lock();
        let previous = std::env::var_os("DOGESH_HERDR_ENABLED");
        unsafe { std::env::set_var("DOGESH_HERDR_ENABLED", "1") };
        let dir = tempfile::tempdir().unwrap();
        let mut proxy = logical_proxy(dir.path());
        // Logically unset: no DOGESH_HERDR_ENABLED in `vars`.
        let (ctx, observer) = observed_context();
        check_runtimes(&ctx, &mut proxy);
        let result = observed_stdout(&observer);
        unsafe {
            match previous {
                Some(value) => std::env::set_var("DOGESH_HERDR_ENABLED", value),
                None => std::env::remove_var("DOGESH_HERDR_ENABLED"),
            }
        }
        assert!(
            result.contains("skip herdr-pane disabled"),
            "logically unset DOGESH_HERDR_ENABLED must disable: {result}"
        );
    }

    /// A logically enabled shell reports the process pane identity: the
    /// two authorities compose without mixing.
    #[test]
    fn herdr_enabled_shell_sees_process_pane_identity() {
        let _lock = crate::chatgpt::tool::execute::tests::env_lock();
        let saved: Vec<(&str, Option<std::ffi::OsString>)> = [
            "DOGESH_HERDR_ENABLED",
            "HERDR_ENV",
            "HERDR_PANE_ID",
            "HERDR_BIN_PATH",
            "DOGESH_HERDR_OWNER_PID",
        ]
        .iter()
        .map(|key| (*key, std::env::var_os(key)))
        .collect();
        unsafe {
            std::env::set_var("HERDR_ENV", "1");
            std::env::set_var("HERDR_PANE_ID", "w9:p9");
            std::env::set_var("HERDR_BIN_PATH", "/opt/herdr-fixture/herdr");
            std::env::remove_var("DOGESH_HERDR_OWNER_PID");
        }
        let dir = tempfile::tempdir().unwrap();
        let mut proxy = logical_proxy(dir.path());
        proxy
            .vars
            .insert("DOGESH_HERDR_ENABLED".to_string(), "1".to_string());
        let (ctx, observer) = observed_context();
        check_runtimes(&ctx, &mut proxy);
        let result = observed_stdout(&observer);
        unsafe {
            for (key, value) in saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
        assert!(
            result.contains("ok herdr-pane active pane=w9:p9"),
            "logical enable + process identity must compose: {result}"
        );
    }
}
