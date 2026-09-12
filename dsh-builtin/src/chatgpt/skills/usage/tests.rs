use super::*;
use std::sync::MutexGuard;

static TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn test_lock() -> MutexGuard<'static, ()> {
    let guard = TEST_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    with_pending(|pending| pending.clear());
    guard
}

#[test]
fn reads_accumulate_into_one_write_per_flush() {
    let _lock = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("demo");
    std::fs::create_dir_all(&skill).unwrap();
    let state = dir.path().join("state").join("skills.json");

    note_read(&skill, SkillScope::User);
    note_read(&skill, SkillScope::User);
    assert!(!state.exists(), "counters must stay buffered until a flush");

    flush_to(&state);

    let stored = read_state(&state).expect("readable state");
    let entry = stored.skills.get(&key(&skill)).expect("record");
    assert_eq!(entry.reads, 2);
    assert_eq!(entry.scope, "user");
    assert!(entry.last_read_ms > 0);
}

#[test]
fn a_flush_with_nothing_buffered_writes_no_file() {
    let _lock = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("skills.json");

    flush_to(&state);

    assert!(!state.exists());
}

#[test]
fn a_record_for_a_removed_skill_is_dropped_on_flush() {
    let _lock = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("gone");
    let state = dir.path().join("skills.json");

    note_read(&skill, SkillScope::User);
    flush_to(&state);

    assert!(
        read_state(&state)
            .expect("readable state")
            .skills
            .is_empty()
    );
}

#[test]
fn a_created_skill_is_attributed_to_the_agent() {
    let _lock = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("made-up");
    std::fs::create_dir_all(&skill).unwrap();
    let state = dir.path().join("skills.json");

    note_write(&skill, SkillScope::Project, true);
    flush_to(&state);

    let stored = read_state(&state).expect("readable state");
    let entry = stored.skills.get(&key(&skill)).expect("record");
    assert_eq!(entry.created_by, "agent");
    assert_eq!(entry.scope, "project");
    assert_eq!(entry.writes, 1);
}

/// A downgrade must leave a newer file exactly as it found it, counters and
/// all - reading it as empty and then flushing over it is how the records
/// disappear.
#[test]
fn a_state_file_from_a_future_version_is_ignored_not_rewritten() {
    let _lock = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("demo");
    std::fs::create_dir_all(&skill).unwrap();
    let state = dir.path().join("skills.json");
    let original = r#"{"version":99,"skills":{"x":{"reads":5}}}"#;
    std::fs::write(&state, original).unwrap();

    assert!(read_state(&state).is_none());

    note_read(&skill, SkillScope::User);
    flush_to(&state);

    assert_eq!(std::fs::read_to_string(&state).unwrap(), original);
}

/// A file that will not parse carries nothing worth preserving.
#[test]
fn an_unreadable_state_file_is_replaced() {
    let _lock = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("demo");
    std::fs::create_dir_all(&skill).unwrap();
    let state = dir.path().join("skills.json");
    std::fs::write(&state, "{ not json").unwrap();

    note_read(&skill, SkillScope::User);
    flush_to(&state);

    assert_eq!(read_state(&state).unwrap().skills.len(), 1);
}

#[test]
fn a_skill_with_no_record_is_never_reported_as_unused() {
    assert!(!is_stale(None, 10 * DAY_MS));
}

/// The skill the agent wrote a minute ago must not be suggested for
/// deletion just because nothing has read it yet.
#[test]
fn a_freshly_created_skill_is_not_stale() {
    let now = 400 * DAY_MS;
    let fresh = SkillUsage {
        created_ms: now - DAY_MS,
        ..SkillUsage::default()
    };
    assert!(!is_stale(Some(&fresh), now));

    let old = SkillUsage {
        created_ms: now - 200 * DAY_MS,
        ..SkillUsage::default()
    };
    assert!(is_stale(Some(&old), now));
}

#[test]
fn a_recent_read_keeps_an_old_skill_alive() {
    let now = 400 * DAY_MS;
    let record = SkillUsage {
        created_ms: now - 300 * DAY_MS,
        reads: 3,
        last_read_ms: now - DAY_MS,
        ..SkillUsage::default()
    };
    assert!(!is_stale(Some(&record), now));
}

/// A clock that went backwards must not make a skill look abandoned.
#[test]
fn a_timestamp_in_the_future_reads_as_zero_days() {
    assert_eq!(days_since(100, 200), Some(0));
    assert_eq!(days_since(100, 0), None);
    let future = SkillUsage {
        last_read_ms: 200,
        ..SkillUsage::default()
    };
    assert!(!is_stale(Some(&future), 100));
}

#[test]
fn archived_and_pinned_survive_a_counter_flush() {
    let _lock = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("demo");
    std::fs::create_dir_all(&skill).unwrap();
    let state = dir.path().join("skills.json");

    note_write(&skill, SkillScope::User, true);
    flush_to(&state);

    let mut file = read_state(&state).unwrap();
    let entry = file.skills.get_mut(&key(&skill)).unwrap();
    entry.archived_ms = 42;
    entry.pinned = true;
    write_state(&state, &file);

    note_read(&skill, SkillScope::User);
    flush_to(&state);

    let after = read_state(&state).unwrap();
    let entry = after.skills.get(&key(&skill)).unwrap();
    assert_eq!(entry.archived_ms, 42);
    assert!(entry.pinned);
    assert_eq!(entry.reads, 1);
}

/// A state file written before these fields existed must still load, and
/// the missing fields must read as "not archived, not pinned" rather
/// than an error - `#[serde(default)]` is what this checks.
#[test]
fn a_state_file_written_before_the_lifecycle_fields_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("skills.json");
    std::fs::write(
        &state,
        r#"{"version":1,"skills":{"x":{"scope":"user","reads":3}}}"#,
    )
    .unwrap();

    let loaded = read_state(&state).expect("still readable");
    let entry = loaded.skills.get("x").unwrap();
    assert_eq!(entry.archived_ms, 0);
    assert!(!entry.pinned);
    assert!(!is_archived(Some(entry)));
}

/// `sweep` itself has no on/off switch - the environment variable gate
/// lives in `chatgpt.rs`, which decides whether to call this at all. So
/// the only thing to prove here is that a skill stale enough to qualify
/// stays unarchived until something actually calls `sweep_at` - writing
/// usage records on its own must never archive anything.
#[test]
fn auto_archive_is_off_unless_the_caller_invokes_it() {
    let _lock = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("demo");
    std::fs::create_dir_all(&skill).unwrap();
    let state = dir.path().join("skills.json");
    let now = 400 * DAY_MS;

    let mut file = UsageFile::default();
    file.skills.insert(
        key(&skill),
        SkillUsage {
            scope: "user".to_string(),
            created_by: "agent".to_string(),
            created_ms: now - 200 * DAY_MS,
            ..SkillUsage::default()
        },
    );
    write_state(&state, &file);

    assert!(
        !is_archived(read_state(&state).unwrap().skills.get(&key(&skill))),
        "a stale skill must not archive itself just by existing"
    );

    let archived = sweep_at(&state, now, UNUSED_AFTER_DAYS);
    assert_eq!(archived, 1);
    assert!(is_archived(
        read_state(&state).unwrap().skills.get(&key(&skill))
    ));
}

#[test]
fn auto_archive_only_touches_agent_written_unpinned_user_skills() {
    let _lock = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("skills.json");
    let now = 400 * DAY_MS;

    let mut file = UsageFile::default();
    file.skills.insert(
        "agent-user".to_string(),
        SkillUsage {
            scope: "user".to_string(),
            created_by: "agent".to_string(),
            created_ms: now - 200 * DAY_MS,
            ..SkillUsage::default()
        },
    );
    file.skills.insert(
        "pinned".to_string(),
        SkillUsage {
            scope: "user".to_string(),
            created_by: "agent".to_string(),
            created_ms: now - 200 * DAY_MS,
            pinned: true,
            ..SkillUsage::default()
        },
    );
    file.skills.insert(
        "user-written".to_string(),
        SkillUsage {
            scope: "user".to_string(),
            created_by: "user".to_string(),
            created_ms: now - 200 * DAY_MS,
            ..SkillUsage::default()
        },
    );
    file.skills.insert(
        "project".to_string(),
        SkillUsage {
            scope: "project".to_string(),
            created_by: "agent".to_string(),
            created_ms: now - 200 * DAY_MS,
            ..SkillUsage::default()
        },
    );
    file.skills.insert(
        "fresh".to_string(),
        SkillUsage {
            scope: "user".to_string(),
            created_by: "agent".to_string(),
            created_ms: now - DAY_MS,
            ..SkillUsage::default()
        },
    );
    write_state(&state, &file);

    let archived = sweep_at(&state, now, UNUSED_AFTER_DAYS);
    assert_eq!(archived, 1);

    let after = read_state(&state).unwrap();
    assert!(after.skills["agent-user"].archived_ms > 0);
    assert_eq!(after.skills["pinned"].archived_ms, 0);
    assert_eq!(after.skills["user-written"].archived_ms, 0);
    assert_eq!(after.skills["project"].archived_ms, 0);
    assert_eq!(after.skills["fresh"].archived_ms, 0);
}

#[test]
fn set_archived_and_set_pinned_round_trip() {
    let _lock = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("demo");
    std::fs::create_dir_all(&skill).unwrap();
    let state = dir.path().join("skills.json");

    note_write(&skill, SkillScope::User, true);
    flush_to(&state);

    assert!(set_flag_at(&state, &skill, |e| e.pinned = true).unwrap());
    assert!(set_flag_at(&state, &skill, |e| e.archived_ms = 7).unwrap());

    let loaded = read_state(&state).unwrap();
    let entry = loaded.skills.get(&key(&skill)).unwrap();
    assert!(entry.pinned);
    assert_eq!(entry.archived_ms, 7);
}

/// `set_flag` (and therefore `set_archived`/`set_pinned`) must not touch
/// a state file whose version this shell does not understand - the same
/// rule every other write here follows.
#[test]
fn set_flag_leaves_a_future_version_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("skills.json");
    let original = r#"{"version":99,"skills":{}}"#;
    std::fs::write(&state, original).unwrap();

    let applied = set_flag_at(&state, Path::new("/tmp/does-not-matter"), |e| {
        e.pinned = true
    })
    .unwrap();
    assert!(!applied);
    assert_eq!(std::fs::read_to_string(&state).unwrap(), original);
}

#[test]
fn wrote_this_turn_reflects_only_the_buffered_writes() {
    let _lock = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("demo");

    assert!(!wrote_this_turn());
    note_read(&skill, SkillScope::User);
    assert!(!wrote_this_turn());
    note_write(&skill, SkillScope::User, false);
    assert!(wrote_this_turn());
}

#[test]
fn read_this_turn_lists_only_directories_actually_read() {
    let _lock = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let read_dir = dir.path().join("read");
    let written_dir = dir.path().join("written");

    note_read(&read_dir, SkillScope::User);
    note_write(&written_dir, SkillScope::User, false);

    let opened = read_this_turn();
    assert_eq!(opened, vec![read_dir]);
}
