use super::*;

fn history_at(dir: &std::path::Path) -> History {
    let mut history = History::new();
    history.db = Some(Db::new(dir.join("history.db")).unwrap());
    history
}

#[test]
fn event_query_missing_context_and_limit_follow_entry_matcher() {
    let dir = tempfile::tempdir().unwrap();
    let history = history_at(dir.path());
    let conn = history.db.as_ref().unwrap().get_connection();
    conn.execute(
        "INSERT INTO command_events(command, started_at, author) VALUES ('missing %_ÉCHO', 1, 'agent-x')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO command_events(command, started_at, author, cwd, session_id, exit_code, duration_ms)
         VALUES ('present %_ÉCHO', 2, 'agent-x', '/fixture', 'session', 0, 5)",
        [],
    ).unwrap();
    drop(conn);
    for scope in [
        HistoryScope::Cwd,
        HistoryScope::Session,
        HistoryScope::Project,
    ] {
        let query = HistoryQuery {
            scope,
            text: Some("%_écho".into()),
            limit: Some(1),
            ..Default::default()
        };
        let events = history
            .search_command_events(Some("agent-x"), &query)
            .unwrap();
        assert_eq!(events.len(), 1, "{scope:?}");
        assert_eq!(events[0].command, "missing %_ÉCHO", "{scope:?}");
    }
    let query = HistoryQuery {
        text: Some("%_écho".into()),
        limit: Some(0),
        ..Default::default()
    };
    assert!(
        history
            .search_command_events(None, &query)
            .unwrap()
            .is_empty()
    );
    assert!(history.command_events(None, 0).unwrap().is_empty());
    let query = HistoryQuery {
        text: Some("%_écho".into()),
        limit: Some(1),
        ..Default::default()
    };
    assert_eq!(
        history.search_command_events(None, &query).unwrap()[0].command,
        "present %_ÉCHO"
    );
}

#[test]
fn event_project_uses_current_markers_and_exact_context_strings() {
    let dir = tempfile::tempdir().unwrap();
    let history = history_at(dir.path());
    // Bound marker lookup inside the fixture, regardless of ancestor repositories.
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    let repo = dir.path().join("repo");
    let nested = repo.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    // Worktree/submodule .git files use the same marker semantics as directories.
    std::fs::write(repo.join(".git"), "gitdir: /synthetic-target\n").unwrap();
    let cwd = nested.to_string_lossy().into_owned();
    let root = repo.to_string_lossy().into_owned();
    let conn = history.db.as_ref().unwrap().get_connection();
    for time in 1..=2 {
        conn.execute(
            "INSERT INTO command_events(command, started_at, author, cwd) VALUES ('synthetic event', ?1, 'agent-x', ?2)",
            rusqlite::params![time, cwd],
        ).unwrap();
    }
    drop(conn);
    let mut query = HistoryQuery {
        scope: HistoryScope::Project,
        current_project: Some(root.clone()),
        limit: Some(10),
        ..Default::default()
    };
    assert_eq!(
        history
            .search_command_events(Some("all"), &query)
            .unwrap()
            .len(),
        2
    );
    query.current_project = Some(format!("{root}/"));
    assert!(
        history
            .search_command_events(None, &query)
            .unwrap()
            .is_empty()
    );
    // No historical project snapshot exists: removing the marker changes classification.
    std::fs::remove_file(repo.join(".git")).unwrap();
    query.current_project = Some(root);
    assert!(
        history
            .search_command_events(None, &query)
            .unwrap()
            .is_empty()
    );
    query.current_project = Some(dir.path().to_string_lossy().into_owned());
    assert_eq!(
        history.search_command_events(None, &query).unwrap().len(),
        2
    );
}
