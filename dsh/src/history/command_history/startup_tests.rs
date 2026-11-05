use super::*;
use crate::run_modes::{
    prepare_command_history, publish_command_history, publish_older_command_history,
};
use std::time::Duration;

fn fixture() -> (tempfile::TempDir, History, Vec<Entry>) {
    let dir = tempfile::tempdir().unwrap();
    let mut loaded = History::new();
    loaded.db = Some(Db::new(dir.path().join("startup.db")).unwrap());
    loaded
        .write_batch(vec![
            ("Older".into(), 10),
            ("Duplicate".into(), 20),
            ("Duplicate".into(), 30),
        ])
        .unwrap();
    let older = prepare_command_history(&mut loaded, 1, 9000);
    (dir, loaded, older)
}

fn flush(shared: &Arc<ParkingMutex<History>>) {
    let (tx, rx) = mpsc::channel();
    assert!(shared.lock().request_reload(move |result| {
        tx.send(result).unwrap();
    }));
    rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    shared.lock().drain_persistence_acks();
}

#[test]
fn startup_publication_preserves_live_writes_outcomes_and_older_phase() {
    let (_dir, loaded, older) = fixture();
    let shared = Arc::new(ParkingMutex::new(History::new_for_startup()));
    let shell_handle = shared.clone();
    {
        let mut live = shared.lock();
        live.write_batch(vec![
            ("Unique".into(), 100),
            ("Duplicate".into(), 101),
            ("Duplicate".into(), 102),
            ("Older".into(), 103),
        ])
        .unwrap();
        live.record_outcome(
            "Unique",
            HistoryMetadata {
                exit_code: Some(7),
                duration_ms: Some(42),
                cwd: Some("/synthetic".into()),
                session_id: Some("startup-session".into()),
                hostname: Some("fixture-host".into()),
                started_at: chrono::Utc::now().timestamp(),
                author: "human".into(),
                output: Some("synthetic output".into()),
                ledger_mode: CommandLedgerMode::Output,
            },
        )
        .unwrap();
    }
    publish_command_history(&shared, &loaded);
    assert!(Arc::ptr_eq(&shared, &shell_handle));
    {
        let live = shell_handle.lock();
        assert!(live.histories.iter().any(|entry| entry.entry == "Unique"));
        assert_eq!(live.pending_persistence.len(), 3);
        assert_eq!(live.normalized_entries.len(), live.histories.len());
        assert!(live.recent_cache.contains(&"Unique".to_string()));
    }
    // Interleave another user write between recent and older publication.
    shared
        .lock()
        .write_batch(vec![("Older".into(), 104)])
        .unwrap();
    publish_older_command_history(&shared, older);
    flush(&shared);
    let live = shared.lock();
    assert!(live.at_end());
    assert!(live.pending_persistence.is_empty());
    assert!(live.startup_writes.is_none());
    assert_eq!(live.histories.len(), 3);
    for (command, count) in [("Unique", 1), ("Duplicate", 4), ("Older", 3)] {
        let entry = live
            .histories
            .iter()
            .find(|entry| entry.entry == command)
            .unwrap();
        assert_eq!(entry.count, count, "in-memory {command}");
        let persisted: i64 = live
            .db
            .as_ref()
            .unwrap()
            .get_connection()
            .query_row(
                "SELECT count FROM command_history WHERE command = ?1",
                [command],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted, count, "SQLite {command}");
    }
    let unique = live
        .histories
        .iter()
        .find(|entry| entry.entry == "Unique")
        .unwrap();
    assert_eq!(unique.exit_code, Some(7));
    assert_eq!(unique.duration_ms, Some(42));
    assert_eq!(unique.session_id.as_deref(), Some("startup-session"));
    let events = live.command_events(Some("all"), 10).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].command, "Unique");
    assert_eq!(events[0].exit_code, Some(7));
    assert_eq!(events[0].cwd.as_deref(), Some("/synthetic"));
    assert_eq!(events[0].duration_ms, Some(42));
    assert_eq!(events[0].session_id.as_deref(), Some("startup-session"));
    assert_eq!(events[0].hostname.as_deref(), Some("fixture-host"));
    assert_eq!(events[0].output.as_deref(), Some("synthetic output"));
    assert!(
        live.normalized_entries
            .iter()
            .all(|text| text.chars().all(|c| !c.is_uppercase()))
    );
    assert_eq!(live.recent_cache.last().map(String::as_str), Some("Older"));
}

#[test]
fn startup_publication_preserves_navigation_in_both_phases() {
    let (_dir, loaded, older) = fixture();
    let shared = Arc::new(ParkingMutex::new(History::new_for_startup()));
    {
        let mut live = shared.lock();
        live.write_batch(vec![("Local".into(), 100)]).unwrap();
        live.search_word = Some("loc".into());
        assert_eq!(live.back().as_deref(), Some("Local"));
    }
    publish_command_history(&shared, &loaded);
    publish_older_command_history(&shared, older);
    let mut live = shared.lock();
    assert_eq!(live.search_word.as_deref(), Some("loc"));
    assert!(!live.at_end());
    assert_eq!(live.histories[live.current_index].entry, "Local");
    assert!(live.back().is_none());
    live.reset_index();
    assert_eq!(live.back().as_deref(), Some("Local"));
    live.reset_index();
    drop(live);
    flush(&shared);
}

#[test]
fn startup_publication_replays_each_outcome_once_and_retires_journal() {
    let (_dir, loaded, older) = fixture();
    let shared = Arc::new(ParkingMutex::new(History::new_for_startup()));
    for code in [1, 2] {
        let mut live = shared.lock();
        live.write_batch(vec![("Older".into(), 100 + i64::from(code))])
            .unwrap();
        live.record_outcome(
            "Older",
            HistoryMetadata {
                exit_code: Some(code),
                started_at: chrono::Utc::now().timestamp(),
                ledger_mode: CommandLedgerMode::Metadata,
                ..Default::default()
            },
        )
        .unwrap();
    }
    publish_command_history(&shared, &loaded);
    flush(&shared);
    // An acknowledged operation must remain accounted for when the older
    // snapshot arrives, with no replay at the second publication boundary.
    publish_older_command_history(&shared, older);
    shared
        .lock()
        .write_batch(vec![("After ready".into(), 200)])
        .unwrap();
    flush(&shared);
    let live = shared.lock();
    assert!(live.startup_writes.is_none());
    assert!(live.pending_persistence.is_empty());
    let events = live.command_events(Some("all"), 10).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .iter()
            .map(|event| event.exit_code.unwrap())
            .sum::<i32>(),
        3
    );
    let older_entry = live
        .histories
        .iter()
        .find(|entry| entry.entry == "Older")
        .unwrap();
    assert_eq!(older_entry.count, 3);
    assert_eq!(older_entry.exit_code, Some(2));
    let count: i64 = live
        .db
        .as_ref()
        .unwrap()
        .get_connection()
        .query_row(
            "SELECT count FROM command_history WHERE command = 'Older'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 3);
}

#[test]
fn ordinary_memory_history_does_not_buffer_startup_operations() {
    let mut history = History::new();
    for _ in 0..10 {
        history.write_history("memory-only").unwrap();
        history
            .record_outcome("memory-only", HistoryMetadata::default())
            .unwrap();
    }
    assert!(history.startup_writes.is_none());
}
