use super::*;
use rusqlite::OptionalExtension;
use std::time::Duration;

fn reject_synthetic_command(db: &Db) {
    db.get_connection()
        .execute_batch(
            "CREATE TRIGGER reject_synthetic_command
             BEFORE INSERT ON command_history
             WHEN NEW.command = 'synthetic rejected'
             BEGIN SELECT RAISE(ABORT, 'synthetic history failure'); END;",
        )
        .unwrap();
}

fn receive_ack(history: &History) -> HistoryPersistAck {
    history
        .persist_ack_rx
        .as_ref()
        .unwrap()
        .lock()
        .recv_timeout(Duration::from_secs(5))
        .expect("background writer must acknowledge the batch")
}

#[test]
fn failed_upsert_rolls_back_the_entire_batch() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::new(dir.path().join("history.db")).unwrap();
    History::write_batch_sync(
        &mut db,
        vec![("synthetic existing".to_string(), 10)],
        Some("original context".to_string()),
    )
    .unwrap();
    reject_synthetic_command(&db);

    let result = History::write_batch_sync(
        &mut db,
        vec![
            ("synthetic existing".to_string(), 20),
            ("synthetic before".to_string(), 21),
            ("synthetic rejected".to_string(), 22),
            ("synthetic after".to_string(), 23),
        ],
        Some("replacement context".to_string()),
    );

    assert!(result.is_err(), "a rejected UPSERT must fail the batch");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("synthetic history failure")
    );
    let conn = db.get_connection();
    let rows = conn
        .prepare("SELECT command, timestamp, context, count FROM command_history ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![(
            "synthetic existing".to_string(),
            10,
            "original context".to_string(),
            1
        )]
    );
}

#[test]
fn background_upsert_failure_ack_retains_the_whole_local_batch() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::new(dir.path().join("history.db")).unwrap();
    reject_synthetic_command(&db);
    let mut history = History::new();
    history.db = Some(db.clone());
    history.start_background_writer();
    history
        .write_batch(vec![
            ("synthetic before".to_string(), 10),
            ("synthetic rejected".to_string(), 20),
            ("synthetic after".to_string(), 30),
        ])
        .unwrap();

    let ack = receive_ack(&history);
    assert!(
        ack.result.is_err(),
        "the writer must report the SQLite failure"
    );
    assert_eq!(ack.commands.len(), 3);
    assert!(
        history
            .pending_persistence
            .values()
            .all(|pending| { pending.persistence_id == ack.persistence_id })
    );
    // Feed the actual writer acknowledgement through the production drain boundary.
    let (ack_tx, ack_rx) = mpsc::channel();
    history.persist_ack_rx = Some(Arc::new(ParkingMutex::new(ack_rx)));
    ack_tx.send(ack).unwrap();
    history.drain_persistence_acks();

    assert_eq!(history.pending_persistence.len(), 3);
    let stored: i64 = db
        .get_connection()
        .query_row("SELECT COUNT(*) FROM command_history", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stored, 0, "no partial batch may reach SQLite");
    let snapshot =
        History::load_reload_snapshot(&db, history.revision, history.pending_entries_snapshot())
            .unwrap();
    assert_eq!(snapshot.histories.len(), 3);
    assert_eq!(
        history.apply_reload_snapshot(snapshot),
        HistoryReloadApply::Applied
    );
    assert_eq!(history.histories.len(), 3);
}

#[test]
fn successful_background_acks_only_clear_the_matching_generation() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::new(dir.path().join("history.db")).unwrap();
    let mut history = History::new();
    history.db = Some(db.clone());
    history.start_background_writer();
    history
        .write_batch(vec![("synthetic repeated".to_string(), 10)])
        .unwrap();
    let first = receive_ack(&history);
    history
        .write_batch(vec![("synthetic repeated".to_string(), 20)])
        .unwrap();
    let second = receive_ack(&history);
    assert!(first.result.is_ok());
    assert!(second.result.is_ok());
    assert_ne!(first.persistence_id, second.persistence_id);
    let newest_id = second.persistence_id;
    let (ack_tx, ack_rx) = mpsc::channel();
    history.persist_ack_rx = Some(Arc::new(ParkingMutex::new(ack_rx)));
    ack_tx.send(first).unwrap();
    history.drain_persistence_acks();
    let pending = history
        .pending_persistence
        .get("synthetic repeated")
        .unwrap();
    assert_eq!(pending.persistence_id, newest_id);
    assert_eq!((pending.entry.count, pending.entry.when), (2, 20));
    ack_tx.send(second).unwrap();
    history.drain_persistence_acks();
    assert!(history.pending_persistence.is_empty());
    let stored = db
        .get_connection()
        .query_row(
            "SELECT count, timestamp FROM command_history WHERE command = 'synthetic repeated'",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap();
    assert_eq!(stored, (2, 20));
}

fn outcome_metadata() -> HistoryMetadata {
    HistoryMetadata {
        exit_code: Some(7),
        duration_ms: Some(123),
        cwd: Some("/synthetic/cwd".to_string()),
        session_id: Some("synthetic-session".to_string()),
        hostname: Some("synthetic-host".to_string()),
        ..HistoryMetadata::default()
    }
}

fn drain_actual_acks(history: &mut History, acks: Vec<HistoryPersistAck>) {
    let (tx, rx) = mpsc::channel();
    history.persist_ack_rx = Some(Arc::new(ParkingMutex::new(rx)));
    for ack in acks {
        tx.send(ack).unwrap();
    }
    history.drain_persistence_acks();
}

fn assert_reload_preserves_entry(history: &mut History, db: &Db, expected: &Entry) {
    let snapshot =
        History::load_reload_snapshot(db, history.revision, history.pending_entries_snapshot())
            .unwrap();
    assert_eq!(
        history.apply_reload_snapshot(snapshot),
        HistoryReloadApply::Applied
    );
    assert_eq!(history.histories.len(), 1);
    // Compare every aggregate and metadata field, including context and timestamp.
    assert_eq!(
        serde_json::to_value(&history.histories[0]).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

fn failed_batch_then_outcome(existing: bool, background: bool) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::new(dir.path().join("history.db")).unwrap();
    let mut history = History::new();
    history.db = Some(db.clone());
    if existing {
        history
            .write_batch(vec![("synthetic rejected".to_string(), 10)])
            .unwrap();
    }
    reject_synthetic_command(&db);
    if background {
        history.start_background_writer();
    }
    history
        .write_batch(vec![("synthetic rejected".to_string(), 20)])
        .unwrap();
    let failed = background.then(|| receive_ack(&history));
    if let Some(ack) = &failed {
        assert!(ack.result.is_err());
    }
    history
        .record_outcome("synthetic rejected", outcome_metadata())
        .unwrap();
    let expected = history.histories[0].clone();
    assert_eq!(expected.count, if existing { 2 } else { 1 });
    if let Some(failed) = failed {
        let outcome = receive_ack(&history);
        assert!(
            outcome.result.is_ok(),
            "metadata UPDATE succeeds even with no row"
        );
        // Both operations ran on the real writer; only ACK delivery is controlled.
        drain_actual_acks(&mut history, vec![failed, outcome]);
    }
    let stored: Option<(i64, Option<i32>)> = db
        .get_connection()
        .query_row(
            "SELECT count, exit_code FROM command_history WHERE command = 'synthetic rejected'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap();
    assert_eq!(stored, existing.then_some((1, Some(7))));
    assert_reload_preserves_entry(&mut history, &db, &expected);
    assert_eq!(history.pending_persistence.len(), 1);
}

#[test]
fn failed_new_batch_survives_successful_outcome_ack_and_reload() {
    failed_batch_then_outcome(false, true);
}

#[test]
fn failed_existing_increment_survives_successful_outcome_ack_and_reload() {
    failed_batch_then_outcome(true, true);
}

#[test]
fn synchronous_failed_batch_survives_successful_outcome_and_reload() {
    failed_batch_then_outcome(false, false);
    failed_batch_then_outcome(true, false);
}

#[test]
fn failed_increments_survive_later_successful_increment_ack_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::new(dir.path().join("history.db")).unwrap();
    let mut history = History::new();
    history.db = Some(db.clone());
    history
        .write_batch(vec![("synthetic rejected".to_string(), 10)])
        .unwrap();
    reject_synthetic_command(&db);
    history.start_background_writer();
    let mut acks = Vec::new();
    for when in 20..28 {
        history
            .write_batch(vec![("synthetic rejected".to_string(), when)])
            .unwrap();
        let ack = receive_ack(&history);
        assert!(ack.result.is_err());
        acks.push(ack);
    }
    db.get_connection()
        .execute_batch("DROP TRIGGER reject_synthetic_command")
        .unwrap();
    // Duplicates are two increments even though the pending snapshot is one entry.
    history
        .write_batch(vec![
            ("synthetic rejected".to_string(), 30),
            ("synthetic rejected".to_string(), 31),
        ])
        .unwrap();
    let success = receive_ack(&history);
    assert!(success.result.is_ok());
    acks.push(success);
    let expected = history.histories[0].clone();
    assert_eq!(expected.count, 11);
    drain_actual_acks(&mut history, acks);
    let stored: i64 = db
        .get_connection()
        .query_row(
            "SELECT count FROM command_history WHERE command = 'synthetic rejected'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, 3);
    assert_reload_preserves_entry(&mut history, &db, &expected);
    assert_eq!(history.pending_persistence.len(), 1);
}

#[test]
fn successful_batch_ack_keeps_newer_outcome_until_its_ack() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::new(dir.path().join("history.db")).unwrap();
    let mut history = History::new();
    history.db = Some(db.clone());
    history.start_background_writer();
    history
        .write_batch(vec![
            ("synthetic repeated".to_string(), 10),
            ("synthetic repeated".to_string(), 20),
        ])
        .unwrap();
    let batch = receive_ack(&history);
    history
        .record_outcome("synthetic repeated", outcome_metadata())
        .unwrap();
    let outcome = receive_ack(&history);
    assert!(batch.result.is_ok());
    assert!(outcome.result.is_ok());
    let expected = history.histories[0].clone();
    drain_actual_acks(&mut history, vec![batch]);
    assert_eq!(history.pending_persistence.len(), 1);
    assert_eq!(history.pending_entries_snapshot()[0].exit_code, Some(7));
    drain_actual_acks(&mut history, vec![outcome]);
    assert!(history.pending_persistence.is_empty());
    assert_reload_preserves_entry(&mut history, &db, &expected);
}

#[test]
fn outcome_ack_without_a_tracked_entry_cannot_confirm_a_later_batch() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::new(dir.path().join("history.db")).unwrap();
    let mut history = History::new();
    history.db = Some(db.clone());
    history.start_background_writer();
    history
        .record_outcome("synthetic new", outcome_metadata())
        .unwrap();
    let outcome = receive_ack(&history);
    assert!(outcome.result.is_ok());
    assert!(history.pending_persistence.is_empty());
    history
        .write_batch(vec![("synthetic new".to_string(), 20)])
        .unwrap();
    let batch = receive_ack(&history);
    assert!(batch.result.is_ok());
    drain_actual_acks(&mut history, vec![outcome]);
    assert_eq!(history.pending_persistence.len(), 1);
    drain_actual_acks(&mut history, vec![batch]);
    assert!(history.pending_persistence.is_empty());
}
