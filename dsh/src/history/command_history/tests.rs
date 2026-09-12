use super::*;

fn ledger_metadata(author: &str, mode: CommandLedgerMode) -> HistoryMetadata {
    HistoryMetadata {
        exit_code: Some(1),
        duration_ms: Some(42),
        cwd: Some("/repo".to_string()),
        session_id: Some("session".to_string()),
        hostname: Some("host".to_string()),
        started_at: chrono::Utc::now().timestamp(),
        author: author.to_string(),
        output: Some("API_KEY=secret".to_string()),
        ledger_mode: mode,
    }
}

#[test]
fn ledger_is_append_only_and_filters_by_author() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = History::new();
    history.db = Some(crate::db::Db::new(dir.path().join("history.db")).unwrap());
    history.write_history("cargo test").unwrap();
    history
        .record_outcome(
            "cargo test",
            ledger_metadata("human", CommandLedgerMode::Metadata),
        )
        .unwrap();
    history
        .record_outcome(
            "cargo test",
            ledger_metadata("agent-x", CommandLedgerMode::Metadata),
        )
        .unwrap();

    let all = history.command_events(Some("all"), 10).unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|event| event.output.is_none()));
    let agent = history.command_events(Some("agent-x"), 10).unwrap();
    assert_eq!(agent.len(), 1);
    assert_eq!(agent[0].author, "agent-x");
}

#[test]
fn output_mode_truncates_on_a_character_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = History::new();
    history.db = Some(crate::db::Db::new(dir.path().join("history.db")).unwrap());
    history.write_history("command").unwrap();
    let mut metadata = ledger_metadata("human", CommandLedgerMode::Output);
    metadata.output = Some("あ".repeat(LEDGER_MAX_OUTPUT_BYTES));
    history.record_outcome("command", metadata).unwrap();
    let event = history.command_events(Some("all"), 1).unwrap().remove(0);
    assert!(event.output.unwrap().ends_with("... (truncated)"));
}

#[test]
fn recording_an_event_prunes_entries_outside_retention_window() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = History::new();
    history.db = Some(crate::db::Db::new(dir.path().join("history.db")).unwrap());
    {
        let conn = history.db.as_ref().unwrap().get_connection();
        conn.execute(
            "INSERT INTO command_events(command, started_at, author) VALUES ('old', 0, 'human')",
            [],
        )
        .unwrap();
    }
    history.write_history("new").unwrap();
    history
        .record_outcome("new", ledger_metadata("human", CommandLedgerMode::Metadata))
        .unwrap();
    let events = history.command_events(Some("all"), 10).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].command, "new");
}

#[test]
fn failure_filter_is_applied_before_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = History::new();
    history.db = Some(crate::db::Db::new(dir.path().join("history.db")).unwrap());
    let conn = history.db.as_ref().unwrap().get_connection();
    conn.execute(
        "INSERT INTO command_events(command, started_at, exit_code, author)
             VALUES ('old failure', 1, 1, 'human')",
        [],
    )
    .unwrap();
    for timestamp in 2..=102 {
        conn.execute(
            "INSERT INTO command_events(command, started_at, exit_code, author)
                 VALUES (?1, ?2, 0, 'human')",
            rusqlite::params![format!("success {timestamp}"), timestamp],
        )
        .unwrap();
    }
    drop(conn);

    let events = history
        .command_events_filtered(Some("all"), 1, true)
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].command, "old failure");
}

#[test]
fn disabled_or_broken_atuin_adapter_never_waits_for_command_execution() {
    let event = CommandEvent {
        id: 0,
        command: "cargo test".to_string(),
        cwd: None,
        started_at: 0,
        duration_ms: None,
        exit_code: None,
        session_id: None,
        hostname: None,
        author: "human".to_string(),
        output: None,
    };
    let start = std::time::Instant::now();
    enqueue_atuin_dual_write_with(
        true,
        std::path::PathBuf::from("/definitely/missing/atuin"),
        event,
    );
    assert!(start.elapsed() < std::time::Duration::from_millis(50));
}

#[test]
fn reload_snapshot_imports_another_sessions_command() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::Db::new(dir.path().join("history.db")).unwrap();
    let mut other_session = History::new();
    other_session.db = Some(db.clone());
    other_session
        .write_batch(vec![("cargo test".to_string(), 100)])
        .unwrap();

    let mut history = History::new();
    history.db = Some(db.clone());
    let snapshot = History::load_reload_snapshot(&db, history.revision, Vec::new()).unwrap();

    assert_eq!(
        history.apply_reload_snapshot(snapshot),
        HistoryReloadApply::Applied
    );
    assert_eq!(history.iter().next().unwrap().entry, "cargo test");
}

#[test]
fn stale_reload_snapshot_does_not_overwrite_local_append() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::Db::new(dir.path().join("history.db")).unwrap();
    let mut history = History::new();
    history.db = Some(db.clone());
    let snapshot = History::load_reload_snapshot(&db, history.revision, Vec::new()).unwrap();

    history.add_test_entry("local while loading");

    assert_eq!(
        history.apply_reload_snapshot(snapshot),
        HistoryReloadApply::Stale
    );
    assert_eq!(history.iter().next().unwrap().entry, "local while loading");
}

#[test]
fn reload_snapshot_is_discarded_during_navigation() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::Db::new(dir.path().join("history.db")).unwrap();
    let mut history = History::new();
    history.db = Some(db.clone());
    history
        .write_batch(vec![("git status".to_string(), 100)])
        .unwrap();
    let snapshot = History::load_reload_snapshot(&db, history.revision, Vec::new()).unwrap();
    assert_eq!(history.back().as_deref(), Some("git status"));

    assert_eq!(
        history.apply_reload_snapshot(snapshot),
        HistoryReloadApply::Navigating
    );
    assert_eq!(history.iter().next().unwrap().entry, "git status");
}

#[test]
fn reload_snapshot_keeps_an_unpersisted_local_entry() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::Db::new(dir.path().join("history.db")).unwrap();
    let local = Entry {
        entry: "local after failed write".to_string(),
        when: 200,
        count: 1,
        context: Some("/repo".to_string()),
        exit_code: Some(1),
        duration_ms: Some(42),
        cwd: Some("/repo".to_string()),
        session_id: Some("local-session".to_string()),
        hostname: Some("local-host".to_string()),
    };

    let snapshot = History::load_reload_snapshot(&db, 0, vec![local.clone()]).unwrap();

    assert_eq!(snapshot.histories.len(), 1);
    let restored = &snapshot.histories[0];
    assert_eq!(restored.entry, local.entry);
    assert_eq!(restored.exit_code, local.exit_code);
    assert_eq!(restored.session_id, local.session_id);
}

#[test]
fn failed_persistence_ack_keeps_the_local_reload_delta() {
    let mut history = History::new();
    history
        .write_batch(vec![("keep after sqlite failure".to_string(), 300)])
        .unwrap();
    let pending = history
        .pending_persistence
        .get("keep after sqlite failure")
        .unwrap();
    let persistence_id = pending.persistence_id;
    let (ack_tx, ack_rx) = mpsc::channel();
    history.persist_ack_rx = Some(Arc::new(ParkingMutex::new(ack_rx)));
    ack_tx
        .send(HistoryPersistAck {
            persistence_id,
            commands: vec!["keep after sqlite failure".to_string()],
            result: Err("disk full".to_string()),
        })
        .unwrap();

    history.drain_persistence_acks();

    let pending = history.pending_entries_snapshot();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].entry, "keep after sqlite failure");
}

fn sample_entry(
    entry: &str,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    cwd: Option<&str>,
    context: Option<&str>,
    session_id: Option<&str>,
) -> Entry {
    Entry {
        entry: entry.to_string(),
        when: Local::now().timestamp(),
        count: 1,
        context: context.map(str::to_string),
        exit_code,
        duration_ms,
        cwd: cwd.map(str::to_string),
        session_id: session_id.map(str::to_string),
        hostname: Some("test-host".to_string()),
    }
}

#[test]
fn search_entries_filters_by_scope_status_and_query() {
    let mut history = History::new();
    history.histories = vec![
        sample_entry(
            "cargo test",
            Some(0),
            Some(1200),
            Some("/repo"),
            Some("/repo"),
            Some("session-a"),
        ),
        sample_entry(
            "cargo build",
            Some(1),
            Some(3200),
            Some("/repo"),
            Some("/repo"),
            Some("session-a"),
        ),
        sample_entry(
            "npm test",
            Some(0),
            Some(800),
            Some("/web"),
            Some("/web"),
            Some("session-b"),
        ),
    ];

    let query = HistoryQuery {
        text: Some("cargo".to_string()),
        scope: HistoryScope::Session,
        status: HistoryStatusFilter::Failure,
        min_duration_ms: Some(1000),
        limit: None,
        current_cwd: Some("/repo".to_string()),
        current_project: Some("/repo".to_string()),
        current_session_id: Some("session-a".to_string()),
    };

    let results = history.search_entries(&query);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry, "cargo build");
}

#[test]
fn entry_matches_applies_scope() {
    let entry = sample_entry(
        "cargo build",
        Some(0),
        Some(100),
        Some("/repo/src"),
        Some("/repo"),
        Some("session-a"),
    );
    let context = HistoryQuery {
        current_cwd: Some("/repo/src".to_string()),
        current_project: Some("/repo".to_string()),
        current_session_id: Some("session-a".to_string()),
        ..Default::default()
    };

    for scope in [
        HistoryScope::Global,
        HistoryScope::Session,
        HistoryScope::Cwd,
        HistoryScope::Project,
    ] {
        let query = HistoryQuery {
            scope,
            ..context.clone()
        };
        assert!(
            EntryMatcher::new(&query).matches(&entry, None),
            "{scope:?} should match its own context"
        );
    }

    let elsewhere = HistoryQuery {
        scope: HistoryScope::Cwd,
        current_cwd: Some("/other".to_string()),
        ..context.clone()
    };
    assert!(!EntryMatcher::new(&elsewhere).matches(&entry, None));

    let other_session = HistoryQuery {
        scope: HistoryScope::Session,
        current_session_id: Some("session-b".to_string()),
        ..context.clone()
    };
    assert!(!EntryMatcher::new(&other_session).matches(&entry, None));

    let other_project = HistoryQuery {
        scope: HistoryScope::Project,
        current_project: Some("/elsewhere".to_string()),
        ..context
    };
    assert!(!EntryMatcher::new(&other_project).matches(&entry, None));
}

#[test]
fn entry_matches_applies_status_and_duration() {
    let ok = sample_entry("ok", Some(0), Some(5000), None, None, None);
    let failed = sample_entry("bad", Some(2), Some(10), None, None, None);
    let unknown = sample_entry("legacy", None, None, None, None, None);

    let success = HistoryQuery {
        status: HistoryStatusFilter::Success,
        ..Default::default()
    };
    assert!(EntryMatcher::new(&success).matches(&ok, None));
    assert!(!EntryMatcher::new(&success).matches(&failed, None));
    assert!(!EntryMatcher::new(&success).matches(&unknown, None));

    let failure = HistoryQuery {
        status: HistoryStatusFilter::Failure,
        ..Default::default()
    };
    assert!(EntryMatcher::new(&failure).matches(&failed, None));
    assert!(!EntryMatcher::new(&failure).matches(&ok, None));
    // An entry with no recorded status is not a known failure.
    assert!(!EntryMatcher::new(&failure).matches(&unknown, None));

    let slow = HistoryQuery {
        min_duration_ms: Some(1000),
        ..Default::default()
    };
    assert!(EntryMatcher::new(&slow).matches(&ok, None));
    assert!(!EntryMatcher::new(&slow).matches(&failed, None));
    assert!(!EntryMatcher::new(&slow).matches(&unknown, None));
}

#[test]
fn entry_matches_text_is_case_insensitive_with_and_without_cache() {
    let entry = sample_entry("Cargo Build", None, None, None, None, None);
    let query = HistoryQuery {
        text: Some("CARGO".to_string()),
        ..Default::default()
    };
    let matcher = EntryMatcher::new(&query);

    assert!(matcher.matches(&entry, None));
    // The cached path must agree with the on-the-fly one.
    assert!(matcher.matches(&entry, Some("cargo build")));
}

#[test]
fn snapshot_entries_returns_newest_first_and_respects_the_cap() {
    let mut history = History::new();
    history
        .write_batch(vec![
            ("first".to_string(), 1),
            ("second".to_string(), 2),
            ("third".to_string(), 3),
        ])
        .unwrap();

    let snapshot = history.snapshot_entries(2);
    assert_eq!(snapshot.len(), 2);
    assert_eq!(snapshot[0].entry, "third");
    assert_eq!(snapshot[1].entry, "second");
}

#[test]
fn search_entries_uses_recent_order_and_limit() {
    let mut history = History::new();
    history
        .write_batch(vec![
            ("Git Status".to_string(), 1),
            ("git commit".to_string(), 2),
            ("cargo test".to_string(), 3),
            ("git checkout main".to_string(), 4),
        ])
        .unwrap();

    let query = HistoryQuery {
        text: Some("GIT".to_string()),
        limit: Some(2),
        ..Default::default()
    };

    let results = history.search_entries(&query);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].entry, "git checkout main");
    assert_eq!(results[1].entry, "git commit");
}

#[test]
fn history_navigation_filters_by_substring_and_restores_end() {
    let mut history = History::new();
    history
        .write_batch(vec![
            ("git status".to_string(), 1),
            ("cargo test".to_string(), 2),
            ("docker status".to_string(), 3),
        ])
        .unwrap();
    history.search_word = Some("status".to_string());

    assert_eq!(history.back().as_deref(), Some("docker status"));
    assert_eq!(history.back().as_deref(), Some("git status"));
    assert_eq!(history.back(), None);
    assert_eq!(history.forward().as_deref(), Some("docker status"));
    assert_eq!(history.forward(), None);
    assert!(history.at_end());
    assert_eq!(history.search_word.as_deref(), Some("status"));
}

#[test]
fn history_navigation_uses_fish_smartcase_matching() {
    let mut history = History::new();
    history
        .write_batch(vec![
            ("Git Status".to_string(), 1),
            ("git status".to_string(), 2),
        ])
        .unwrap();

    history.search_word = Some("status".to_string());
    assert_eq!(history.back().as_deref(), Some("git status"));
    assert_eq!(history.back().as_deref(), Some("Git Status"));

    history.reset_index();
    history.search_word = Some("Status".to_string());
    assert_eq!(history.back().as_deref(), Some("Git Status"));
    assert_eq!(history.back(), None);
}

#[test]
fn history_navigation_no_match_keeps_index_stable() {
    let mut history = History::new();
    history
        .write_batch(vec![
            ("git status".to_string(), 1),
            ("cargo test".to_string(), 2),
        ])
        .unwrap();
    history.search_word = Some("deploy".to_string());

    assert_eq!(history.back(), None);
    assert!(history.at_end());
    assert_eq!(history.forward(), None);
    assert!(history.at_end());
}
