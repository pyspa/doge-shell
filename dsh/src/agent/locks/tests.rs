use super::*;
use crate::agent::SqliteTaskStore;
use dsh_builtin::ShellProxy;

fn shell() -> crate::shell::Shell {
    crate::shell::Shell::new(crate::environment::Environment::new())
}

#[test]
fn a_free_task_admits_and_a_held_one_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let mut shell = shell();
    let Admission::Admitted(lock) = admit_run(&mut shell, &store, "task-a").unwrap() else {
        panic!("expected admission");
    };
    assert!(matches!(
        admit_run(&mut shell, &store, "task-a").unwrap(),
        Admission::TaskBusy
    ));
    drop(lock);
    assert!(matches!(
        admit_run(&mut shell, &store, "task-a").unwrap(),
        Admission::Admitted(_)
    ));
}

#[test]
fn the_ceiling_is_respected_across_different_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let mut shell = shell();
    // A shell variable, not an OS-level env var: `AI_AGENT_MAX_CONCURRENT`
    // must resolve the same way the sibling budget settings do (shell var,
    // then environment) - this doubles as the regression test for that.
    shell.set_var("AI_AGENT_MAX_CONCURRENT".into(), "1".into());

    let Admission::Admitted(first) = admit_run(&mut shell, &store, "task-a").unwrap() else {
        panic!("expected admission");
    };
    assert!(matches!(
        admit_run(&mut shell, &store, "task-b").unwrap(),
        Admission::NoFreeSlot
    ));

    shell.set_var("AI_AGENT_MAX_CONCURRENT".into(), "2".into());
    let Admission::Admitted(second) = admit_run(&mut shell, &store, "task-b").unwrap() else {
        panic!("expected admission with a raised ceiling");
    };
    assert!(matches!(
        admit_run(&mut shell, &store, "task-c").unwrap(),
        Admission::NoFreeSlot
    ));

    drop(first);
    drop(second);
}

#[test]
fn try_lock_task_does_not_care_about_other_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let _first = try_lock_task(&store, "task-a").unwrap().unwrap();
    // A different task's lock is unaffected by another task's lock, unlike
    // the old global `active.lock`.
    assert!(try_lock_task(&store, "task-b").unwrap().is_some());
    assert!(try_lock_task(&store, "task-a").unwrap().is_none());
}

/// A collision with someone else's momentary probe (`is_locked` takes the
/// lock and immediately releases it) must not be mistaken for a real,
/// sustained holder - `try_lock_task` retries a few times before giving up.
#[test]
fn try_lock_task_retries_through_a_brief_collision() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let lock = try_lock_task(&store, "task-a").unwrap().unwrap();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(3));
        drop(lock);
    });
    // Held for longer than a single attempt but well inside the retry
    // window - without retrying, this would report `TaskBusy` outright.
    assert!(try_lock_task(&store, "task-a").unwrap().is_some());
    releaser.join().unwrap();
}

#[test]
fn dropping_the_lock_releases_it_for_the_next_admission() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let lock = try_lock_task(&store, "task-a").unwrap().unwrap();
    drop(lock);
    assert!(try_lock_task(&store, "task-a").unwrap().is_some());
}

#[test]
fn orphaned_lock_files_are_pruned_but_live_ones_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let _unknown = try_lock_task(&store, "gone").unwrap().unwrap();
    drop(_unknown); // unlocked, but the file itself remains on disk
    let held = try_lock_task(&store, "known").unwrap().unwrap();

    prune_orphaned_locks(&store, &["known".to_string()]);

    assert!(!lock_path(&store, "gone").exists());
    assert!(lock_path(&store, "known").exists());
    drop(held);
}
