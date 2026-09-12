use super::*;
use crate::test_support::TestShellProxy;
use dsh_types::Context;
use std::path::Path;
use tempfile::tempdir;

/// The crate-wide lock, shared with every other module that scopes
/// `XDG_CONFIG_HOME`/`XDG_STATE_HOME` for a test, so two test threads
/// never race to set the same environment variable.
fn with_homes<R>(config: &Path, state: &Path, f: impl FnOnce() -> R) -> R {
    let _lock = crate::chatgpt::tool::execute::tests::env_lock();
    let prev_config = std::env::var_os("XDG_CONFIG_HOME");
    let prev_state = std::env::var_os("XDG_STATE_HOME");
    // SAFETY: single-threaded under `env_lock`.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", config);
        std::env::set_var("XDG_STATE_HOME", state);
    }
    let result = f();
    unsafe {
        match prev_config {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match prev_state {
            Some(value) => std::env::set_var("XDG_STATE_HOME", value),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }
    result
}

fn project(dir: &Path) -> PathBuf {
    let root = std::fs::canonicalize(dir).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    root
}

fn write_project_skill(root: &Path, name: &str, description: &str) -> PathBuf {
    let dir = root.join(".dsh/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n\nbody\n"),
    )
    .unwrap();
    dir
}

fn ctx() -> Context {
    let pid = nix::unistd::getpid();
    Context::new_safe(pid, pid, false)
}

fn proxy(cwd: PathBuf) -> TestShellProxy {
    TestShellProxy {
        current_dir: cwd,
        confirm_result: true,
        ..TestShellProxy::default()
    }
}

fn stage_project_proposal(
    root: &Path,
    name: &str,
    contents: &str,
    base_digest: Option<String>,
) -> String {
    pending::stage(Proposal {
        version: 0,
        id: pending::proposal_id(skills::SkillScope::Project, name, "SKILL.md"),
        scope: "project".to_string(),
        name: name.to_string(),
        file: "SKILL.md".to_string(),
        action: if base_digest.is_some() {
            "write_file"
        } else {
            "create"
        }
        .to_string(),
        project_root: Some(root.join(".dsh/skills")),
        contents: contents.to_string(),
        base_digest,
        created_ms: usage::now_ms(),
        origin: "tool".to_string(),
        note: Some("because reasons".to_string()),
    })
    .unwrap()
}

#[test]
fn approve_writes_through_the_same_path_as_the_tool() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let config = tempdir().unwrap();
    let state = tempdir().unwrap();

    with_homes(config.path(), state.path(), || {
        let contents = skill_tool::render_skill_md("demo", "Use when demoing", "step one\n");
        let id = stage_project_proposal(&root, "demo", &contents, None);

        let context = ctx();
        let mut p = proxy(root.clone());
        let status = approve(&context, &mut p, &id);

        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert_eq!(
            std::fs::read_to_string(root.join(".dsh/skills/demo/SKILL.md")).unwrap(),
            contents
        );
        assert!(pending::list().0.is_empty());

        let records = usage::load();
        let record = records
            .get(&usage::key(&root.join(".dsh/skills/demo")))
            .expect("a write records usage");
        assert_eq!(record.created_by, "agent");
    });
}

/// A `create` proposal's staleness check alone cannot see a *sibling*
/// collision - `request.target` (`demo/SKILL.md`) still does not exist
/// either way, so `base_digest` still matches. `create()` itself refuses
/// this before writing; `approve` must refuse it too, not reproduce the
/// "two skills, one name" collision through a different door.
#[test]
fn approve_refuses_a_create_that_would_collide_with_a_file_skill_staged_after_it() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let config = tempdir().unwrap();
    let state = tempdir().unwrap();

    with_homes(config.path(), state.path(), || {
        let contents = skill_tool::render_skill_md("demo", "Use when demoing", "step\n");
        let id = stage_project_proposal(&root, "demo", &contents, None);

        // A file-skill named `demo.md` appears after staging but before
        // approval - through a different tool call, a hand edit, or a
        // second approved proposal.
        std::fs::create_dir_all(root.join(".dsh/skills")).unwrap();
        std::fs::write(
            root.join(".dsh/skills/demo.md"),
            "---\nname: demo\ndescription: unrelated file skill\n---\n",
        )
        .unwrap();

        let context = ctx();
        let mut p = proxy(root.clone());
        let status = approve(&context, &mut p, &id);

        assert_eq!(status, ExitStatus::ExitedWith(1));
        assert!(
            !root.join(".dsh/skills/demo").exists(),
            "the colliding directory must never be created"
        );
        assert_eq!(
            pending::list().0.len(),
            1,
            "a refused approval leaves the proposal for review, not silently dropped"
        );
    });
}

/// The same collision, but the directory itself (not a sibling `.md`)
/// was created by some other route between staging and approval.
#[test]
fn approve_refuses_a_create_whose_directory_appeared_after_it_was_staged() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let config = tempdir().unwrap();
    let state = tempdir().unwrap();

    with_homes(config.path(), state.path(), || {
        let contents = skill_tool::render_skill_md("demo", "Use when demoing", "step\n");
        let id = stage_project_proposal(&root, "demo", &contents, None);

        // The skill directory exists (created some other way), but not
        // yet `SKILL.md` itself - so `base_digest` (keyed on the target
        // file, not the directory) still reads as unchanged.
        std::fs::create_dir_all(root.join(".dsh/skills/demo")).unwrap();
        std::fs::write(root.join(".dsh/skills/demo/notes.txt"), "unrelated").unwrap();

        let context = ctx();
        let mut p = proxy(root.clone());
        let status = approve(&context, &mut p, &id);

        assert_eq!(status, ExitStatus::ExitedWith(1));
        assert!(!root.join(".dsh/skills/demo/SKILL.md").exists());
        assert_eq!(pending::list().0.len(), 1);
    });
}

/// A target that moved since staging (edited by hand, or by a second
/// proposal already approved) must be refused, not clobbered.
#[test]
fn approve_refuses_a_proposal_whose_target_changed_since_it_was_staged() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let config = tempdir().unwrap();
    let state = tempdir().unwrap();

    with_homes(config.path(), state.path(), || {
        let original = "---\nname: demo\ndescription: original\n---\n\nbody\n".to_string();
        std::fs::create_dir_all(root.join(".dsh/skills/demo")).unwrap();
        std::fs::write(root.join(".dsh/skills/demo/SKILL.md"), &original).unwrap();

        // Staged against a digest that does not match what is on disk -
        // as if the file had been edited since.
        let stale_contents =
            skill_tool::render_skill_md("demo", "a change nobody reviewed yet", "step\n");
        let id = stage_project_proposal(
            &root,
            "demo",
            &stale_contents,
            Some(pending::content_digest("something else entirely")),
        );

        let context = ctx();
        let mut p = proxy(root.clone());
        let status = approve(&context, &mut p, &id);

        assert_eq!(status, ExitStatus::ExitedWith(1));
        assert_eq!(
            std::fs::read_to_string(root.join(".dsh/skills/demo/SKILL.md")).unwrap(),
            original,
            "a stale approval must not overwrite what is actually there"
        );
        assert_eq!(
            pending::list().0.len(),
            1,
            "a refused approval leaves the proposal for review, not silently dropped"
        );
    });
}

#[test]
fn approve_refuses_a_project_proposal_from_another_repository() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let other_dir = tempdir().unwrap();
    let other_root = project(other_dir.path());
    let config = tempdir().unwrap();
    let state = tempdir().unwrap();

    with_homes(config.path(), state.path(), || {
        let contents = skill_tool::render_skill_md("demo", "Use when demoing", "step\n");
        // Staged as if it came from `other_root`, but approved from `root`.
        let id = pending::stage(Proposal {
            version: 0,
            id: pending::proposal_id(skills::SkillScope::Project, "demo", "SKILL.md"),
            scope: "project".to_string(),
            name: "demo".to_string(),
            file: "SKILL.md".to_string(),
            action: "create".to_string(),
            project_root: Some(other_root.join(".dsh/skills")),
            contents,
            base_digest: None,
            created_ms: usage::now_ms(),
            origin: "tool".to_string(),
            note: None,
        })
        .unwrap();

        let context = ctx();
        let mut p = proxy(root.clone());
        let status = approve(&context, &mut p, &id);

        assert_eq!(status, ExitStatus::ExitedWith(1));
        assert!(!root.join(".dsh/skills/demo").exists());
        assert_eq!(pending::list().0.len(), 1);
    });
}

#[test]
fn reject_removes_the_proposal_without_writing_anything() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let config = tempdir().unwrap();
    let state = tempdir().unwrap();

    with_homes(config.path(), state.path(), || {
        let contents = skill_tool::render_skill_md("demo", "Use when demoing", "step\n");
        let id = stage_project_proposal(&root, "demo", &contents, None);

        let context = ctx();
        let mut p = proxy(root.clone());
        let status = reject(&context, &mut p, &id);

        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert!(pending::list().0.is_empty());
        assert!(!root.join(".dsh/skills/demo").exists());
    });
}

#[test]
fn pending_cmd_lists_broken_proposals_without_dropping_them() {
    let config = tempdir().unwrap();
    let state = tempdir().unwrap();

    with_homes(config.path(), state.path(), || {
        std::fs::create_dir_all(crate::config_paths::skills_pending_dir()).unwrap();
        std::fs::write(
            crate::config_paths::skills_pending_dir().join("junk.json"),
            "{ not json",
        )
        .unwrap();

        let context = ctx();
        let status = pending_cmd(&context);
        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert_eq!(pending::list().1.len(), 1, "the broken file must survive");
    });
}

#[test]
fn archive_refuses_a_project_skill() {
    let dir = tempdir().unwrap();
    let root = project(dir.path());
    let config = tempdir().unwrap();
    let state = tempdir().unwrap();

    with_homes(config.path(), state.path(), || {
        write_project_skill(&root, "demo", "Use when demoing");

        let context = ctx();
        let mut p = proxy(root.clone());
        let status = archive(&context, &mut p, "demo");

        assert_eq!(status, ExitStatus::ExitedWith(1));
        assert!(!usage::is_archived(
            usage::load().get(&usage::key(&root.join(".dsh/skills/demo")))
        ),);
    });
}

#[test]
fn archive_and_unarchive_round_trip_for_a_personal_skill() {
    let config = tempdir().unwrap();
    let state = tempdir().unwrap();
    let cwd = tempdir().unwrap();
    let cwd_path = std::fs::canonicalize(cwd.path()).unwrap();

    with_homes(config.path(), state.path(), || {
        let skill_dir = config.path().join("dsh/skills/demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: Use when demoing\n---\n\nbody\n",
        )
        .unwrap();

        let context = ctx();
        let mut p = proxy(cwd_path.clone());

        assert_eq!(archive(&context, &mut p, "demo"), ExitStatus::ExitedWith(0));
        assert!(usage::is_archived(
            usage::load().get(&usage::key(&skill_dir))
        ));

        assert_eq!(
            unarchive(&context, &mut p, "demo"),
            ExitStatus::ExitedWith(0)
        );
        assert!(!usage::is_archived(
            usage::load().get(&usage::key(&skill_dir))
        ));
    });
}

#[test]
fn pin_prevents_a_skill_from_being_reported_as_pinned_is_false_by_default() {
    let config = tempdir().unwrap();
    let state = tempdir().unwrap();
    let cwd = tempdir().unwrap();
    let cwd_path = std::fs::canonicalize(cwd.path()).unwrap();

    with_homes(config.path(), state.path(), || {
        let skill_dir = config.path().join("dsh/skills/demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: Use when demoing\n---\n\nbody\n",
        )
        .unwrap();

        let context = ctx();
        let mut p = proxy(cwd_path.clone());

        assert!(
            !usage::load()
                .get(&usage::key(&skill_dir))
                .is_some_and(|r| r.pinned)
        );
        assert_eq!(pin(&context, &mut p, "demo"), ExitStatus::ExitedWith(0));
        assert!(usage::load().get(&usage::key(&skill_dir)).unwrap().pinned);
        assert_eq!(unpin(&context, &mut p, "demo"), ExitStatus::ExitedWith(0));
        assert!(!usage::load().get(&usage::key(&skill_dir)).unwrap().pinned);
    });
}

#[test]
fn an_unread_skill_reads_as_never() {
    assert_eq!(describe_age(1_000_000, 0), "never");
}

#[test]
fn age_is_reported_in_whole_days() {
    let day = 24 * 60 * 60 * 1000u64;
    assert_eq!(describe_age(10 * day, 10 * day - 1), "today");
    assert_eq!(describe_age(10 * day, 9 * day), "1 day ago");
    assert_eq!(describe_age(10 * day, 3 * day), "7 days ago");
}

/// A clock that went backwards must not read as an ancient timestamp.
#[test]
fn a_timestamp_in_the_future_reads_as_today() {
    assert_eq!(describe_age(100, 200), "today");
}

#[test]
fn a_summary_within_the_column_budget_is_unchanged() {
    assert_eq!(display_summary("short trigger"), "short trigger");
}

#[test]
fn a_summary_at_the_prompt_budget_is_still_cut_for_the_terminal() {
    // The prompt's own budget (240) is wider than a terminal column
    // budget should be; `skill list` has its own, smaller limit.
    let long = "a".repeat(MAX_LIST_SUMMARY_CHARS + 40);
    let shown = display_summary(&long);
    assert!(shown.ends_with("..."));
    assert!(shown.chars().count() <= MAX_LIST_SUMMARY_CHARS + 3);
}

#[test]
fn unified_lines_diffs_a_small_change_line_by_line() {
    let diff = unified_lines("a\nb\nc\n", "a\nx\nc\n");
    assert!(diff.contains("- b"), "{diff}");
    assert!(diff.contains("+ x"), "{diff}");
    assert!(diff.contains("  a"), "{diff}");
}

/// An on-disk file has no size cap the way a staged proposal's body
/// does, so an unusually large one must fall back rather than build an
/// `O(n*m)` table sized by it.
#[test]
fn unified_lines_falls_back_instead_of_building_an_unbounded_table() {
    let huge = "line\n".repeat(MAX_DIFF_LINES + 1);
    let diff = unified_lines(&huge, "new content\n");
    assert!(diff.contains("too large to diff"), "{diff}");
    assert!(diff.contains("new content"), "{diff}");
}
