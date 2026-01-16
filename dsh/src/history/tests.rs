//! Tests for history module.

use super::*;
use chrono::Local;
use dsh_frecency::{FrecencyStore, SortMethod};
use std::path::PathBuf;
use std::sync::Arc;

fn init() {
    let _ = tracing_subscriber::fmt::try_init();
}

#[test]
fn test_new() {
    init();
    let history = History::from_file("dsh_cmd_history").unwrap();
    assert!(history.db.is_some());
}

#[test]
fn test_open() -> anyhow::Result<()> {
    init();
    let mut history = History::from_file("dsh_cmd_history")?;
    let history = history.open()?;
    history.close()
}

#[test]
fn test_write_batch() -> anyhow::Result<()> {
    init();
    // Clear test file if exists
    let test_name = "dsh_test_batch";
    if let Ok(path) = crate::environment::get_data_file(format!("{}.db", test_name).as_str()) {
        let _ = std::fs::remove_file(path);
    }

    let mut history = History::from_file(test_name)?;

    let now = Local::now().timestamp();
    let entries = vec![
        ("ls".to_string(), now - 10),
        ("cd".to_string(), now - 5),
        ("ls".to_string(), now),
    ];

    history.write_batch(entries)?;

    assert_eq!(history.histories.len(), 2); // "ls" should be deduplicated

    let ls_entry = history.histories.iter().find(|e| e.entry == "ls").unwrap();
    assert_eq!(ls_entry.count, 2);
    assert_eq!(ls_entry.when, now); // Should have the latest timestamp

    let cd_entry = history.histories.iter().find(|e| e.entry == "cd").unwrap();
    assert_eq!(cd_entry.count, 1);

    Ok(())
}

#[test]
fn test_load() -> anyhow::Result<()> {
    init();
    let mut history = History::from_file("dsh_cmd_history")?;
    let s = history.load()?;
    tracing::debug!("loaded {:?}", s);
    Ok(())
}

#[test]
#[ignore]
fn test_back() -> anyhow::Result<()> {
    init();
    let cmd1 = "docker";
    let cmd2 = "docker-compose";

    let mut history = History::from_file("dsh_cmd_history")?;

    let s = history.load()?;
    tracing::debug!("loaded {:?}", s);

    history.write_history(cmd1)?;
    history.write_history(cmd2)?;

    if let Some(h) = history.back() {
        assert_eq!(cmd2, h);
    } else {
        panic!("failed read history");
    }
    if let Some(h) = history.back() {
        assert_eq!(cmd1, h);
    } else {
        panic!("failed read history");
    }

    Ok(())
}

#[test]
#[ignore]
fn frecency() -> anyhow::Result<()> {
    init();
    let mut history = FrecencyHistory::from_file("dsh_frecency_history")?;
    history.add("git");
    history.add("git");
    std::thread::sleep(std::time::Duration::from_millis(100));
    history.add("git checkout");
    let recent = history.sorted(&SortMethod::Recent);
    assert_eq!(recent[0].item, "git checkout");
    assert_eq!(recent[1].item, "git");

    let frequent = history.sorted(&SortMethod::Frequent);
    assert_eq!(frequent[0].item, "git");
    assert_eq!(frequent[1].item, "git checkout");

    let first = history.search_prefix("gi").unwrap();
    assert_eq!(first, "git checkout");

    history.add("git checkout origin master");
    history.add("git config --list");
    history.add("git switch config");
    history.show_score("gc");

    Ok(())
}

#[test]
fn print_item() -> anyhow::Result<()> {
    init();
    let mut history = FrecencyHistory::from_file("dsh_frecency_history")?;
    history.add("git status");
    history.add("git checkout");

    let vec = history.sort_by_match("gsta");
    let mut out = std::io::stdout().lock();
    for item in vec {
        item.print(&mut out);
    }

    Ok(())
}

#[test]
fn test_frecency_completion() -> anyhow::Result<()> {
    init();
    let temp_dir = tempfile::tempdir()?;
    let _file_path = temp_dir.path().join("frecency_test_history");

    let mut history = FrecencyHistory::new();
    history.store = Some(Arc::new(dsh_frecency::FrecencyStore::default()));

    for _ in 0..5 {
        history.add("frequent_cmd");
    }

    history.add("recent_cmd");

    history.add("cmd_frequent");
    history.add("cmd_frequent");
    history.add("cmd_frequent");
    history.add("cmd_recent");

    let result = history.search_prefix("cmd");
    assert_eq!(result, Some("cmd_frequent".to_string()));

    let result_recent = history.search_prefix("cmd_r");
    assert_eq!(result_recent, Some("cmd_recent".to_string()));

    Ok(())
}

#[test]
fn test_save_no_path() -> anyhow::Result<()> {
    init();
    let mut history = FrecencyHistory::new();
    assert!(history.db.is_none());
    history.save()?;
    Ok(())
}

#[test]
fn test_context_aware_boosting() -> anyhow::Result<()> {
    init();
    let temp_dir = tempfile::tempdir()?;
    let dir_a = temp_dir.path().join("a");
    let dir_b = temp_dir.path().join("b");
    let dir_a_str = dir_a.to_string_lossy().to_string();
    let dir_b_str = dir_b.to_string_lossy().to_string();

    let _history_file = temp_dir.path().join("history_context");
    let mut history = FrecencyHistory::new();
    let _path = PathBuf::from("history_context.db");
    history.store = Some(Arc::new(dsh_frecency::FrecencyStore::default()));

    if let Some(ref mut store) = history.store {
        let store_mut = Arc::make_mut(store);
        store_mut.add("cmd_common", Some(dir_a_str.clone()));
        store_mut.add("cmd_unique_a", Some(dir_a_str.clone()));
        store_mut.add("cmd_common", Some(dir_b_str.clone()));
        store_mut.add("cmd_unique_b", Some(dir_b_str.clone()));
    }

    let result = history.search_prefix_with_context("cmd_unique", Some(&dir_a_str));
    assert_eq!(result, Some("cmd_unique_a".to_string()));

    let result_b = history.search_prefix_with_context("cmd_unique", Some(&dir_b_str));
    assert_eq!(result_b, Some("cmd_unique_b".to_string()));

    Ok(())
}

#[test]
fn test_arc_cow_behavior() -> anyhow::Result<()> {
    init();
    let mut history = FrecencyHistory::new();
    let mut store = dsh_frecency::FrecencyStore::default();
    store.add("initial_cmd", None);
    store.changed = true;
    history.store = Some(Arc::new(store));

    let snapshot_arc = Arc::clone(history.store.as_ref().unwrap());

    history.add("new_cmd");

    if let Some(ref mut s) = history.store {
        let s_mut = Arc::make_mut(s);
        s_mut.changed = false;
    }

    let snapshot = snapshot_arc.as_ref();
    let current = history.store.as_ref().unwrap().as_ref();

    assert!(snapshot.items.iter().any(|i| i.item == "initial_cmd"));
    assert!(!snapshot.items.iter().any(|i| i.item == "new_cmd"));
    assert!(snapshot.changed);
    assert!(current.items.iter().any(|i| i.item == "initial_cmd"));
    assert!(current.items.iter().any(|i| i.item == "new_cmd"));
    assert!(!current.changed);
    assert!(!std::ptr::eq(snapshot, current));

    Ok(())
}

#[test]
fn test_async_history_update() {
    use parking_lot::Mutex;
    use std::sync::{Arc, Barrier};
    use std::thread;

    let history = Arc::new(Mutex::new(FrecencyHistory::new()));
    let history_clone = history.clone();

    let barrier = Arc::new(Barrier::new(2));
    let barrier_clone = barrier.clone();

    let handle = thread::spawn(move || {
        barrier_clone.wait();
        let mut store = FrecencyStore::default();
        store.add("async_cmd", None);
        let mut guard = history_clone.lock();
        guard.store = Some(Arc::new(store));
    });

    assert!(
        history.lock().store.is_none() || history.lock().store.as_ref().unwrap().items.is_empty()
    );

    barrier.wait();
    handle.join().unwrap();

    let guard = history.lock();
    assert!(guard.store.is_some());
    assert!(
        guard
            .store
            .as_ref()
            .unwrap()
            .items
            .iter()
            .any(|i| i.item == "async_cmd")
    );
}

#[test]
fn test_background_writer() -> anyhow::Result<()> {
    init();
    let db_file = "dsh_test_background_writer.db";
    let db_path = crate::environment::get_data_file(db_file)?;
    let _ = std::fs::remove_file(&db_path);

    let mut history = History::from_file("dsh_test_background_writer")?;
    history.start_background_writer();

    let entries = vec![
        ("cmd_async_1".to_string(), 1000),
        ("cmd_async_2".to_string(), 1001),
    ];
    history.write_batch(entries)?;

    std::thread::sleep(std::time::Duration::from_millis(500));

    let db = crate::db::Db::new(db_path)?;
    let conn = db.get_connection();
    let mut stmt =
        conn.prepare("SELECT command, timestamp, count FROM command_history WHERE command = ?1")?;

    let row: (String, i64, i64) =
        stmt.query_row(["cmd_async_1"], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    assert_eq!(row.0, "cmd_async_1");
    assert_eq!(row.1, 1000);
    assert_eq!(row.2, 1);

    let row2: (String, i64, i64) =
        stmt.query_row(["cmd_async_2"], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    assert_eq!(row2.0, "cmd_async_2");
    assert_eq!(row2.1, 1001);

    Ok(())
}

#[test]
fn test_frecency_reload() -> anyhow::Result<()> {
    init();
    let db_name = "test_frecency_reload";
    let db_file = "test_frecency_reload.db";
    let db_path = crate::environment::get_data_file(db_file)?;
    let _ = std::fs::remove_file(&db_path);

    let mut history_a = FrecencyHistory::from_file(db_name)?;
    let mut history_b = FrecencyHistory::from_file(db_name)?;

    history_a.add("/tmp/path_a");
    history_a.force_changed();
    history_a.save()?;

    {
        let db = crate::db::Db::new(db_path.clone())?;
        let conn = db.get_connection();
        let mut stmt =
            conn.prepare("SELECT count(*) FROM directory_snapshot WHERE path = '/tmp/path_a'")?;
        let count: i64 = stmt.query_row([], |r| r.get(0))?;
        assert_eq!(count, 1, "DB should have path_a");
    }

    assert!(
        history_b
            .store
            .as_ref()
            .unwrap()
            .items
            .iter()
            .all(|i| i.item != "/tmp/path_a")
    );

    history_b.reload()?;

    assert!(
        history_b
            .store
            .as_ref()
            .unwrap()
            .items
            .iter()
            .any(|i| i.item == "/tmp/path_a")
    );

    history_b.add("/tmp/path_b");
    history_b.force_changed();
    history_b.save()?;

    history_a.reload()?;

    assert!(
        history_a
            .store
            .as_ref()
            .unwrap()
            .items
            .iter()
            .any(|i| i.item == "/tmp/path_b")
    );
    assert!(
        history_a
            .store
            .as_ref()
            .unwrap()
            .items
            .iter()
            .any(|i| i.item == "/tmp/path_a")
    );

    Ok(())
}
