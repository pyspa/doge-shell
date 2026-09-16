use super::*;
use crate::shell_capabilities::ApprovalDecision;
use crate::test_support::TestShellProxy;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::tempdir;

type TestProxy = TestShellProxy;

/// `skills_dir()` is read from the environment, so a test that writes a
/// personal skill has to own `XDG_CONFIG_HOME` for its duration.
fn with_config_home<R>(dir: &Path, f: impl FnOnce() -> R) -> R {
    let _lock = crate::chatgpt::tool::execute::tests::env_lock();
    let previous = std::env::var_os("XDG_CONFIG_HOME");
    // SAFETY: single-threaded under `env_lock`.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", dir) };
    let result = f();
    match previous {
        Some(value) => unsafe { std::env::set_var("XDG_CONFIG_HOME", value) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }
    result
}

fn project(dir: &Path) -> PathBuf {
    let root = std::fs::canonicalize(dir).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    root
}

fn proxy(cwd: PathBuf) -> TestProxy {
    TestProxy {
        current_dir: cwd,
        confirm_result: true,
        ..TestProxy::default()
    }
}

/// `skills_pending_dir()` is read from the environment too, so a test
/// that stages a proposal needs the same kind of scoped override.
fn with_state_home<R>(dir: &Path, f: impl FnOnce() -> R) -> R {
    let _lock = crate::chatgpt::tool::execute::tests::env_lock();
    let previous = std::env::var_os("XDG_STATE_HOME");
    // SAFETY: single-threaded under `env_lock`.
    unsafe { std::env::set_var("XDG_STATE_HOME", dir) };
    let result = f();
    match previous {
        Some(value) => unsafe { std::env::set_var("XDG_STATE_HOME", value) },
        None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
    }
    result
}

/// A task with no `--write` grant for the target used to stall on
/// `InputRequired` the same as an interactive denial. With the default
/// staging mode (`task`), it stages a proposal and keeps running
/// instead - the one behaviour change this whole feature makes.
#[test]
fn a_task_without_a_write_grant_stages_instead_of_stalling() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let state = tempdir().unwrap();

    with_state_home(state.path(), || {
        let runtime = crate::test_support::test_runtime(&root);
        let mut p = TestProxy {
            current_dir: root.clone(),
            agent_runtime: Some(runtime.clone()),
            ..TestProxy::default()
        };

        let result = run(
                r#"{"action":"create","name":"demo","scope":"project","description":"d","body":"step"}"#,
                &mut p,
            )
            .expect("staging must not return an error");

        assert!(result.contains("\"staged\""), "{result}");
        assert!(!root.join(".dogesh/skills/demo").exists());
        assert_eq!(
            runtime.lock().task.status,
            dsh_types::agent::TaskStatus::Running,
            "the task must not be stopped for a person to look at"
        );

        let (proposals, _broken) = skills::pending::list();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].origin, "tool");
        assert_eq!(proposals[0].name, "demo");
    });
}

/// The interactive path must be completely unaffected by the default
/// staging mode - only a task with no grant is redirected.
#[test]
fn an_interactive_turn_is_unchanged_by_the_default_staging_mode() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let state = tempdir().unwrap();

    with_state_home(state.path(), || {
        let mut p = proxy(root.clone());

        let result = run(
                r#"{"action":"create","name":"demo","scope":"project","description":"d","body":"step"}"#,
                &mut p,
            )
            .unwrap();

        assert!(!result.contains("\"staged\""), "{result}");
        assert!(root.join(".dogesh/skills/demo/SKILL.md").is_file());
        assert!(skills::pending::list().0.is_empty());
    });
}

/// `AI_CHAT_SKILL_STAGING=always` stages every write, interactive or
/// not - for a person who wants to review each one before it lands.
#[test]
fn staging_always_queues_an_interactive_write_without_touching_disk() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let state = tempdir().unwrap();

    with_state_home(state.path(), || {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut p = TestProxy {
            current_dir: root.clone(),
            confirm_counter: Some(calls.clone()),
            confirm_result: true,
            vars: [("AI_CHAT_SKILL_STAGING".to_string(), "always".to_string())]
                .into_iter()
                .collect(),
            ..TestProxy::default()
        };

        let result = run(
                r#"{"action":"create","name":"demo","scope":"project","description":"d","body":"step"}"#,
                &mut p,
            )
            .unwrap();

        assert!(result.contains("\"staged\""), "{result}");
        assert!(!root.join(".dogesh/skills/demo").exists());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a staged write must not ask - approval happens later, at `skill approve`"
        );
        assert_eq!(skills::pending::list().0.len(), 1);
    });
}

/// A write that the lint would reject must never reach the queue -
/// `skill approve` would only fail on it later, after a person already
/// spent a look on it.
#[test]
fn a_lint_rejection_is_refused_before_it_can_be_staged() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let state = tempdir().unwrap();

    with_state_home(state.path(), || {
        let mut p = TestProxy {
            current_dir: root.clone(),
            vars: [("AI_CHAT_SKILL_STAGING".to_string(), "always".to_string())]
                .into_iter()
                .collect(),
            ..TestProxy::default()
        };

        let err = run(
            r#"{"action":"create","name":"nameless","scope":"project","body":"x"}"#,
            &mut p,
        )
        .expect_err("a skill with no description must not be staged");
        assert!(err.contains("description"), "{err}");
        assert!(skills::pending::list().0.is_empty());
    });
}

/// Staging must not touch the counters or the cache a real write
/// updates - nothing was written yet, so nothing about it should look
/// used or fresh.
#[test]
fn staging_does_not_bump_the_usage_counters_or_clear_the_cache() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let state = tempdir().unwrap();

    with_state_home(state.path(), || {
        let mut p = TestProxy {
            current_dir: root.clone(),
            vars: [("AI_CHAT_SKILL_STAGING".to_string(), "always".to_string())]
                .into_iter()
                .collect(),
            ..TestProxy::default()
        };

        run(
                r#"{"action":"create","name":"demo","scope":"project","description":"d","body":"step"}"#,
                &mut p,
            )
            .unwrap();

        assert!(
            skills::usage::load().is_empty(),
            "a staged write is not a usage event"
        );
    });
}

/// `delete` is never staged: there is nothing to review, and it is the
/// one change here with no undo.
#[test]
fn delete_is_never_staged() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let state = tempdir().unwrap();

    with_state_home(state.path(), || {
        // Create the skill normally first, so `delete` has something to
        // refuse to stage rather than failing earlier on "no such skill".
        let mut interactive = proxy(root.clone());
        run(
                r#"{"action":"create","name":"demo","scope":"project","description":"d","body":"step"}"#,
                &mut interactive,
            )
            .unwrap();

        let runtime = crate::test_support::test_runtime(&root);
        let mut p = TestProxy {
            current_dir: root.clone(),
            agent_runtime: Some(runtime.clone()),
            ..TestProxy::default()
        };

        let err = run(
            r#"{"action":"delete","name":"demo","scope":"project"}"#,
            &mut p,
        )
        .expect_err("delete under a task with no grant must stall, not stage");
        assert!(err.contains("agent: permission required"), "{err}");
        assert!(skills::pending::list().0.is_empty());
        assert!(
            root.join(".dogesh/skills/demo").exists(),
            "nothing must actually be deleted"
        );
    });
}

/// This schema is sent on every turn whether or not a skill is ever
/// touched, and it was the largest of the nine - bigger than `search`,
/// nearly twice `execute`. Detail belongs in the errors, which are only
/// paid for when something actually goes wrong.
#[test]
fn the_schema_stays_small_enough_to_carry_every_turn() {
    let rendered = definition().to_string();
    assert!(
        rendered.len() < 1400,
        "skill_manage schema is {} bytes",
        rendered.len()
    );
}

#[test]
fn create_writes_skill_md_with_generated_frontmatter() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root.clone());

    let result = run(
            r##"{"action":"create","name":"rust-bisect","scope":"project","description":"Use when a regression must be located in git history","body":"# Rust bisect\n\n1. build\n"}"##,
            &mut p,
        )
        .unwrap();

    let path = root.join(".dogesh/skills/rust-bisect/SKILL.md");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.starts_with("---\nname: rust-bisect\n"));
    assert!(
        written.contains("description: Use when a regression must be located in git history\n")
    );
    assert!(written.contains("1. build"));
    assert!(result.contains("rust-bisect"), "{result}");
}

/// The generated frontmatter has to survive the reader it was written for.
#[test]
fn a_created_description_with_a_colon_round_trips_through_the_frontmatter_reader() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root.clone());

    run(
            r#"{"action":"create","name":"deploy","scope":"project","description":"Use for: staging and prod","body":"x"}"#,
            &mut p,
        )
        .unwrap();

    let manager = skills::SkillsManager::with_roots(vec![skills::SkillRoot {
        scope: SkillScope::Project,
        origin: skills::SkillOrigin::Dsh,
        path: root.join(".dogesh/skills"),
    }]);
    let loaded = manager.load_skills();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].summary(), "Use for: staging and prod");
}

#[test]
fn create_is_refused_without_a_description() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root);

    let err = run(
        r#"{"action":"create","name":"nameless","scope":"project","body":"x"}"#,
        &mut p,
    )
    .expect_err("a skill with no trigger is invisible");
    assert!(err.contains("description"), "{err}");
}

#[test]
fn a_name_with_a_slash_or_uppercase_is_refused() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root);

    for name in ["../escape", "Upper", "trailing-", "double--hyphen", ""] {
        let args =
            json!({"action": "create", "name": name, "scope": "project", "description": "d"});
        let err =
            run(&args.to_string(), &mut p).expect_err("bad skill name must not reach the disk");
        assert!(
            err.contains("skill name") || err.contains("requires"),
            "{name}: {err}"
        );
    }
}

#[test]
fn a_file_escaping_the_skill_directory_is_refused() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root);

    let err = run(
            r#"{"action":"write_file","name":"demo","scope":"project","file":"../../pwned.md","contents":"x"}"#,
            &mut p,
        )
        .expect_err("a traversal must not escape");
    assert!(err.contains("inside the skill directory"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_symlink_inside_the_skill_directory_cannot_escape_it() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let outside = tempdir().unwrap();
    let skill = root.join(".dogesh/skills/demo");
    std::fs::create_dir_all(&skill).unwrap();
    symlink(outside.path(), skill.join("refs")).unwrap();
    let mut p = proxy(root);

    let err = run(
            r#"{"action":"write_file","name":"demo","scope":"project","file":"refs/pwned.md","contents":"x"}"#,
            &mut p,
        )
        .expect_err("a symlink must not lead out of the skill");
    assert!(err.contains("outside the skill directory"), "{err}");
    assert!(!outside.path().join("pwned.md").exists());
}

#[test]
fn a_denied_answer_writes_nothing() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut p = TestProxy {
        current_dir: root.clone(),
        confirm_counter: Some(calls.clone()),
        confirm_result: false,
        ..TestProxy::default()
    };

    let result = run(
        r#"{"action":"create","name":"demo","scope":"project","description":"d"}"#,
        &mut p,
    )
    .unwrap();

    assert_eq!(result, "Skill change cancelled by user.");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!root.join(".dogesh/skills/demo").exists());
}

/// One "always" per file, shared with `edit`: the user approved the file,
/// not the tool that happened to write it.
#[test]
fn an_always_answer_uses_the_same_write_key_as_edit() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut p = TestProxy {
        current_dir: root.clone(),
        confirm_counter: Some(calls.clone()),
        approval_decision: Some(ApprovalDecision::AllowAlways),
        ..TestProxy::default()
    };

    run(
        r#"{"action":"create","name":"demo","scope":"project","description":"d","body":"one"}"#,
        &mut p,
    )
    .unwrap();
    run(
            r#"{"action":"patch","name":"demo","scope":"project","old_string":"one","new_string":"two"}"#,
            &mut p,
        )
        .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let written = std::fs::read_to_string(root.join(".dogesh/skills/demo/SKILL.md")).unwrap();
    assert!(written.contains("two"));

    let key = super::super::write_approval_key(
        &std::fs::canonicalize(root.join(".dogesh/skills/demo/SKILL.md")).unwrap(),
    );
    assert!(p.agent_session_allowlist.contains(&key), "{key}");
}

/// A standing "always write this file" must not quietly authorise removing
/// the skill it belongs to.
#[test]
fn delete_uses_its_own_key_so_a_write_always_does_not_cover_it() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut p = TestProxy {
        current_dir: root.clone(),
        confirm_counter: Some(calls.clone()),
        approval_decision: Some(ApprovalDecision::AllowAlways),
        ..TestProxy::default()
    };

    run(
        r#"{"action":"create","name":"demo","scope":"project","description":"d"}"#,
        &mut p,
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    run(
        r#"{"action":"delete","name":"demo","scope":"project"}"#,
        &mut p,
    )
    .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 2, "delete must ask again");
    assert!(!root.join(".dogesh/skills/demo").exists());
}

#[test]
fn patch_requires_a_unique_match() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root.clone());

    run(
            r#"{"action":"create","name":"demo","scope":"project","description":"d","body":"step\nstep\n"}"#,
            &mut p,
        )
        .unwrap();

    let err = run(
            r#"{"action":"patch","name":"demo","scope":"project","old_string":"step","new_string":"done"}"#,
            &mut p,
        )
        .expect_err("an ambiguous patch is a different edit than intended");
    assert!(err.contains("appears 2 times"), "{err}");
}

/// A `patch` is applied before it is linted, so this proves the lint sees
/// the result of the edit, not the input to it.
#[test]
fn a_patch_that_removes_the_description_is_refused() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root.clone());

    run(
            r#"{"action":"create","name":"demo","scope":"project","description":"Use when demoing","body":"step one\n"}"#,
            &mut p,
        )
        .unwrap();
    let before = std::fs::read_to_string(root.join(".dogesh/skills/demo/SKILL.md")).unwrap();
    let calls_before = p.confirm_calls;

    let err = run(
            r#"{"action":"patch","name":"demo","scope":"project","old_string":"description: Use when demoing\n","new_string":""}"#,
            &mut p,
        )
        .expect_err("a skill with no description falls out of the prompt");
    assert!(err.contains("description"), "{err}");

    // Refused before anyone was asked, and before anything on disk moved.
    assert_eq!(p.confirm_calls, calls_before);
    let after = std::fs::read_to_string(root.join(".dogesh/skills/demo/SKILL.md")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn a_patch_that_renames_the_frontmatter_name_is_refused() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root.clone());

    run(
            r#"{"action":"create","name":"demo","scope":"project","description":"Use when demoing","body":"step\n"}"#,
            &mut p,
        )
        .unwrap();

    let err = run(
            r#"{"action":"patch","name":"demo","scope":"project","old_string":"name: demo","new_string":"name: renamed"}"#,
            &mut p,
        )
        .expect_err("a frontmatter name that no longer matches its directory is reported broken, not shown");
    assert!(err.contains("does not match"), "{err}");
}

#[test]
fn a_write_file_of_skill_md_without_frontmatter_is_refused() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root.clone());

    run(
            r#"{"action":"create","name":"demo","scope":"project","description":"Use when demoing","body":"step\n"}"#,
            &mut p,
        )
        .unwrap();

    let err = run(
            r#"{"action":"write_file","name":"demo","scope":"project","contents":"just some text, no frontmatter"}"#,
            &mut p,
        )
        .expect_err("SKILL.md without frontmatter has no description to trigger on");
    assert!(err.contains("frontmatter"), "{err}");
}

#[test]
fn an_indented_description_is_refused_because_the_reader_cannot_see_it() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root.clone());

    let contents =
        "---\nname: demo\nmetadata:\n  description: nested under metadata\n---\n\nbody\n";
    let args = json!({
        "action": "write_file",
        "name": "demo",
        "scope": "project",
        "contents": contents,
    });
    let err = run(&args.to_string(), &mut p)
        .expect_err("a nested description is invisible to the flat reader");
    assert!(err.contains("indented under another key"), "{err}");
}

/// Long enough to blow the prompt budget but short of the hard limit: the
/// write goes through, and the model is told why the trigger may not fire.
#[test]
fn a_long_description_is_written_but_warned_about() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root.clone());

    // Longer than the prompt's display budget (240) but within the hard
    // write limit (300): the write goes through, with a warning.
    let description = "Use when ".to_string() + &"x".repeat(260);
    let args = json!({
        "action": "create",
        "name": "demo",
        "scope": "project",
        "description": description,
        "body": "step",
    });
    let result = run(&args.to_string(), &mut p).unwrap();
    assert!(result.contains("warnings"), "{result}");
    assert!(root.join(".dogesh/skills/demo/SKILL.md").exists());
}

/// `references/`, `scripts/` and `assets/` are never parsed for
/// frontmatter, so the same content that would fail as `SKILL.md` is fine
/// here.
#[test]
fn a_bundled_reference_file_is_not_linted_as_a_skill() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root.clone());

    run(
            r#"{"action":"create","name":"demo","scope":"project","description":"Use when demoing","body":"step\n"}"#,
            &mut p,
        )
        .unwrap();

    let args = json!({
        "action": "write_file",
        "name": "demo",
        "scope": "project",
        "file": "references/notes.md",
        "contents": "just some notes, no frontmatter here",
    });
    run(&args.to_string(), &mut p).unwrap();
    assert!(
        root.join(".dogesh/skills/demo/references/notes.md")
            .exists()
    );
}

#[test]
fn project_scope_is_refused_without_a_project() {
    let dir = tempdir().unwrap();
    let plain = std::fs::canonicalize(dir.path()).unwrap();
    let mut p = proxy(plain);

    let err = run(
        r#"{"action":"create","name":"demo","scope":"project","description":"d"}"#,
        &mut p,
    )
    .expect_err("there is no project here");
    assert!(err.contains("no project here"), "{err}");
}

/// `.agents/skills` is shared with other tools. It is read, never written:
/// a directory this shell does not own is not a place for it to leave
/// files, and `scope: "project"` has exactly one destination.
#[test]
fn project_scope_writes_to_dsh_skills_and_never_to_the_agents_root() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    std::fs::create_dir_all(root.join(".agents/skills")).unwrap();
    let mut p = proxy(root.clone());

    run(
        r#"{"action":"create","name":"demo","scope":"project","description":"d"}"#,
        &mut p,
    )
    .expect("create");

    assert!(root.join(".dogesh/skills/demo/SKILL.md").is_file());
    assert!(!root.join(".agents/skills/demo").exists());
    // The tool takes no third scope, so there is no spelling that reaches
    // the shared root at all.
    assert!(
        run(
            r#"{"action":"create","name":"other","scope":"project-agents","description":"d"}"#,
            &mut p,
        )
        .is_err()
    );
}

/// A write that fails after the directory is made left `create` saying
/// "already exists" and `patch` saying "failed to read" - nowhere to go.
#[cfg(unix)]
#[test]
fn a_failed_create_leaves_no_directory_behind() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let skills = root.join(".dogesh/skills");
    std::fs::create_dir_all(&skills).unwrap();
    // Nothing can be created underneath, so the write fails after the
    // request has been validated and approved.
    std::fs::set_permissions(&skills, std::fs::Permissions::from_mode(0o500)).unwrap();
    let mut p = proxy(root.clone());

    let err = run(
        r#"{"action":"create","name":"doomed","scope":"project","description":"d"}"#,
        &mut p,
    )
    .expect_err("the write cannot succeed");

    std::fs::set_permissions(&skills, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(err.contains("failed to write"), "{err}");
    assert!(
        !skills.join("doomed").exists(),
        "a half-made skill must not block the retry"
    );
}

/// `foo/` beside `foo.md` is two skills with one name; the loader has to
/// pick and the other disappears.
#[test]
fn create_refuses_to_shadow_an_existing_file_skill() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let skills = root.join(".dogesh/skills");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(skills.join("deploy.md"), "---\ndescription: d\n---\n").unwrap();
    let mut p = proxy(root);

    let err = run(
        r#"{"action":"create","name":"deploy","scope":"project","description":"d"}"#,
        &mut p,
    )
    .expect_err("two skills would answer to one name");
    assert!(err.contains("already exists beside it"), "{err}");
}

#[test]
fn create_refuses_to_overwrite_an_existing_skill() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root.clone());

    run(
        r#"{"action":"create","name":"demo","scope":"project","description":"first"}"#,
        &mut p,
    )
    .unwrap();
    let err = run(
        r#"{"action":"create","name":"demo","scope":"project","description":"second"}"#,
        &mut p,
    )
    .expect_err("create must not silently replace a skill");
    assert!(err.contains("already exists"), "{err}");
}

#[test]
fn user_scope_writes_into_the_configuration_directory() {
    let config = tempdir().unwrap();
    let cwd = tempdir().unwrap();
    let cwd_path = std::fs::canonicalize(cwd.path()).unwrap();

    with_config_home(config.path(), || {
        let mut p = proxy(cwd_path.clone());
        run(
            r#"{"action":"create","name":"portable","description":"works anywhere"}"#,
            &mut p,
        )
        .unwrap();
    });

    assert!(
        config
            .path()
            .join("dogesh/skills/portable/SKILL.md")
            .is_file()
    );
}

#[test]
fn deleting_skill_md_by_name_points_at_deleting_the_skill() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let mut p = proxy(root);

    let err = run(
        r#"{"action":"delete","name":"demo","scope":"project","file":"SKILL.md"}"#,
        &mut p,
    )
    .expect_err("removing SKILL.md leaves a directory that is not a skill");
    assert!(err.contains("omit `file`"), "{err}");
}

#[test]
fn a_description_that_starts_like_yaml_syntax_is_quoted() {
    assert_eq!(quote_if_needed("plain text"), "plain text");
    assert_eq!(
        quote_if_needed("- looks like a list"),
        "\"- looks like a list\""
    );
    assert_eq!(quote_if_needed("\"quoted\""), "\"\\\"quoted\\\"\"");
}
