use super::*;

fn temp_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::new(dir.path().join("frecency.db")).unwrap();
    (dir, db)
}

fn item<'a>(history: &'a FrecencyHistory, path: &str) -> Option<&'a ItemStats> {
    history
        .store
        .as_ref()
        .unwrap()
        .items
        .iter()
        .find(|item| item.item == path)
}

fn score(history: &FrecencyHistory, path: &str) -> f32 {
    item(history, path).expect("item").get_frecency()
}

fn snapshot_paths(db: &Db) -> Vec<String> {
    let conn = db.get_connection();
    let mut stmt = conn
        .prepare("SELECT path FROM directory_snapshot ORDER BY path")
        .unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

#[test]
fn startup_visits_merge_with_the_snapshot_and_persist_once() {
    let (_dir, db) = temp_db();
    let mut loaded = FrecencyHistory::from_db(db.clone()).unwrap();
    loaded.write_history("/shared").unwrap();
    loaded.start_background_writer();

    let mut live = FrecencyHistory::new_for_startup();
    live.add("/shared");
    let at = dsh_frecency::current_time_secs() - 60.0;
    live.record_visit(DirectoryVisit {
        path: "/early".into(),
        at,
        context: Some("early-context".into()),
        half_life: live.store.as_ref().unwrap().half_life,
    });
    live.save_background();
    assert!(live.prune().is_err());
    live.install_startup_history(loaded);
    assert_eq!(item(&live, "/shared").unwrap().num_accesses(), 2);
    let early = item(&live, "/early").unwrap();
    assert!((early.last_accessed_time() - at).abs() < 0.01);
    assert_eq!(early.context.as_deref(), Some("early-context"));

    live.reload().unwrap();
    assert_eq!(item(&live, "/shared").unwrap().num_accesses(), 2);
    assert_eq!(item(&live, "/early").unwrap().num_accesses(), 1);
    let visits: i64 = db
        .get_connection()
        .query_row("SELECT COUNT(*) FROM directory_visits", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(visits, 3);
}

#[test]
fn failed_startup_keeps_visits_in_memory() {
    let mut live = FrecencyHistory::new_for_startup();
    live.add("/early");
    live.end_startup_initialization();
    live.add("/early");
    assert_eq!(item(&live, "/early").unwrap().num_accesses(), 2);
    assert!(live.startup_visits.is_none());
    assert!(live.pending.lock().is_empty());
}

#[test]
fn saved_score_is_the_current_score_not_the_time_scaled_one() {
    let (_dir, db) = temp_db();
    let mut history = FrecencyHistory::from_db(db.clone()).unwrap();
    // A session that started three half-lives ago keeps its in-memory
    // frecency scaled by 2^3 relative to the current score.
    {
        let store = Arc::make_mut(history.store.as_mut().unwrap());
        store.reference_time -= f64::from(store.half_life) * 3.0;
    }
    history.add("/frecency/a");
    let before = score(&history, "/frecency/a");
    assert!((before - 1.0).abs() < 0.01, "score {before}");
    history.save().unwrap();

    let reloaded = FrecencyHistory::from_db(db.clone()).unwrap();
    let after = score(&reloaded, "/frecency/a");
    assert!((after - before).abs() < 0.01, "{before} became {after}");
}

#[test]
fn non_finite_scores_are_neither_loaded_nor_saved() {
    let (_dir, db) = temp_db();
    db.get_connection()
        .execute(
            "INSERT INTO directory_snapshot \
             (path, score, last_accessed, access_count, half_life, context) \
             VALUES ('/frecency/inf', ?1, 0, 3, 43200.0, NULL)",
            [f64::INFINITY],
        )
        .unwrap();

    let mut history = FrecencyHistory::from_db(db.clone()).unwrap();
    assert!(score(&history, "/frecency/inf").is_finite());

    history.add("/frecency/nan");
    {
        let store = Arc::make_mut(history.store.as_mut().unwrap());
        let nan = store
            .items
            .iter_mut()
            .find(|item| item.item == "/frecency/nan")
            .unwrap();
        nan.set_frecency(f32::NAN);
    }
    history.save().expect("a NaN score must not fail the save");

    let reloaded = FrecencyHistory::from_db(db.clone()).unwrap();
    assert!(score(&reloaded, "/frecency/inf").is_finite());
    assert!(score(&reloaded, "/frecency/nan").is_finite());
}

#[test]
fn unrepresentable_snapshot_half_lives_do_not_block_other_visits() {
    let (_dir, db) = temp_db();
    let now = dsh_frecency::current_time_secs();
    let fallback = f64::from(FrecencyStore::default().half_life);
    for (path, half_life) in [("/tiny", 1.0e-300), ("/huge", 1.0e300)] {
        db.get_connection()
            .execute(
                "INSERT INTO directory_snapshot
             (path, score, last_accessed, access_count, half_life, score_time)
             VALUES (?1, 2, ?2, 2, ?3, ?2)",
                rusqlite::params![path, now - fallback, half_life],
            )
            .unwrap();
    }
    let mut history = FrecencyHistory::from_db(db).unwrap();
    history.add("/other");
    for path in ["/tiny", "/huge"] {
        assert!((score(&history, path) - 1.0).abs() < 0.01);
        history.add(path);
    }
    history.reload().unwrap();
    assert_eq!(item(&history, "/other").unwrap().num_accesses(), 1);
    for path in ["/tiny", "/huge"] {
        assert!((score(&history, path) - 2.0).abs() < 0.01);
    }
}

#[test]
fn legacy_inflated_scores_are_capped_at_the_access_count() {
    let (_dir, db) = temp_db();
    // Older builds wrote a session-relative `last_accessed` and the
    // time-scaled score, e.g. 2^20 after a long session.
    db.get_connection()
        .execute(
            "INSERT INTO directory_snapshot \
             (path, score, last_accessed, access_count, half_life, context) \
             VALUES ('/frecency/legacy', 1048576.0, 3600.5, 4, 43200.0, NULL)",
            [],
        )
        .unwrap();

    let history = FrecencyHistory::from_db(db.clone()).unwrap();
    assert_eq!(score(&history, "/frecency/legacy"), 4.0);
}

#[test]
fn recent_visits_survive_a_restart_with_their_access_stats() {
    let (_dir, db) = temp_db();
    let mut history = FrecencyHistory::from_db(db.clone()).unwrap();
    let mut other = FrecencyHistory::from_db(db.clone()).unwrap();
    history.add("/frecency/recent");
    history.add("/frecency/recent");
    let accessed = item(&history, "/frecency/recent")
        .unwrap()
        .last_accessed_time();
    history.save().unwrap();

    let reloaded = FrecencyHistory::from_db(db.clone()).unwrap();
    let restored = item(&reloaded, "/frecency/recent").expect("dropped on load");
    assert_eq!(restored.num_accesses(), 2);
    assert!((restored.last_accessed_time() - accessed).abs() < 5.0);

    other.reload().unwrap();
    let restored = item(&other, "/frecency/recent").expect("dropped on reload");
    assert_eq!(restored.num_accesses(), 2);
    assert!((restored.last_accessed_time() - accessed).abs() < 5.0);
}

#[test]
fn pruned_paths_are_removed_from_the_database() {
    let (dir, db) = temp_db();
    let kept = dir.path().display().to_string();
    let inaccessible = dir.path().join("loop").display().to_string();
    std::os::unix::fs::symlink(&inaccessible, &inaccessible).unwrap();
    let missing = dir.path().join("missing").display().to_string();
    let mut history = FrecencyHistory::from_db(db.clone()).unwrap();
    history.add(&kept);
    history.add(&inaccessible);
    history.add(&missing);
    history.save().unwrap();
    assert_eq!(
        snapshot_paths(&db),
        [kept.as_str(), inaccessible.as_str(), missing.as_str()]
    );

    history.prune().unwrap();
    history.save().unwrap();
    assert_eq!(snapshot_paths(&db), [kept.as_str(), inaccessible.as_str()]);

    history.reload().unwrap();
    assert!(item(&history, &missing).is_none());
    let reloaded = FrecencyHistory::from_db(db.clone()).unwrap();
    assert!(item(&reloaded, &missing).is_none());
    assert!(item(&reloaded, &kept).is_some());
}

#[test]
fn retrying_a_failed_prune_keeps_a_recreated_directory() {
    let (dir, db) = temp_db();
    let path = dir.path().join("recreated");
    let path = path.to_str().unwrap();
    let mut first = FrecencyHistory::from_db(db.clone()).unwrap();
    first.write_history(path).unwrap();
    db.get_connection()
        .execute_batch(
            "CREATE TRIGGER reject BEFORE DELETE ON directory_snapshot
                        BEGIN SELECT RAISE(ABORT, 'delete rejected'); END;",
        )
        .unwrap();
    assert!(first.prune().is_err());

    std::fs::create_dir(path).unwrap();
    let mut other =
        FrecencyHistory::from_db(Db::new(dir.path().join("frecency.db")).unwrap()).unwrap();
    other.write_history(path).unwrap();
    db.get_connection()
        .execute_batch("DROP TRIGGER reject")
        .unwrap();
    first.reload().unwrap();
    assert_eq!(
        item(&first, path).expect("lost fresh visit").num_accesses(),
        2
    );
}

#[test]
fn directory_visits_keep_only_the_latest_rows() {
    let (_dir, db) = temp_db();
    {
        let conn = db.get_connection();
        for ts in 0..5 {
            persistence::log_visit(&conn, "/frecency/visit", ts, None, 3).unwrap();
        }
    }
    let conn = db.get_connection();
    let mut stmt = conn
        .prepare("SELECT timestamp FROM directory_visits ORDER BY id")
        .unwrap();
    let timestamps: Vec<i64> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(timestamps, [2, 3, 4]);
}

#[test]
fn concurrent_sessions_preserve_visits_across_save_and_reload() {
    let (dir, db) = temp_db();
    let mut a = FrecencyHistory::from_db(db.clone()).unwrap();
    a.write_history("/shared").unwrap();
    let mut b = FrecencyHistory::from_db(Db::new(dir.path().join("frecency.db")).unwrap()).unwrap();
    b.start_background_writer();
    a.write_history("/shared").unwrap();
    b.add("/other");
    b.save_background();
    b.reload().unwrap();
    assert_eq!(item(&b, "/shared").unwrap().num_accesses(), 2);
    b.add("/shared");
    a.write_history("/shared").unwrap();
    b.save_background();
    b.reload().unwrap();
    assert_eq!(item(&b, "/shared").unwrap().num_accesses(), 4);
    assert!((score(&b, "/shared") - 4.0).abs() < 0.01);
    assert_eq!(item(&b, "/other").unwrap().num_accesses(), 1);
}

#[test]
fn failed_visit_transaction_retries_without_counting_twice() {
    let (_dir, db) = temp_db();
    let mut history = FrecencyHistory::from_db(db.clone()).unwrap();
    db.get_connection()
        .execute_batch(
            "CREATE TRIGGER reject BEFORE INSERT ON directory_visits
        BEGIN SELECT RAISE(ABORT, 'visit rejected'); END;",
        )
        .unwrap();
    assert!(history.write_history("/retry").is_err());
    assert!(snapshot_paths(&db).is_empty());
    db.get_connection()
        .execute_batch("DROP TRIGGER reject")
        .unwrap();
    history.save().unwrap();
    history.reload().unwrap();
    assert_eq!(item(&history, "/retry").unwrap().num_accesses(), 1);
}

#[test]
fn legacy_database_is_upgraded_on_visit_and_dated_scores_decay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE directory_snapshot (
            path TEXT PRIMARY KEY, score REAL, last_accessed INTEGER, access_count INTEGER,
            half_life REAL, context TEXT);
            INSERT INTO directory_snapshot VALUES ('/old', 2.0, 1700000000, 3, 43200, NULL);",
        )
        .unwrap();
    }
    let db = Db::new(path).unwrap();
    let mut history = FrecencyHistory::from_db(db.clone()).unwrap();
    history.write_history("/old").unwrap();
    db.get_connection()
        .execute(
            "UPDATE directory_snapshot SET score_time = score_time - half_life",
            [],
        )
        .unwrap();
    history.reload().unwrap();
    assert_eq!(item(&history, "/old").unwrap().num_accesses(), 4);
    assert!((score(&history, "/old") - 1.5).abs() < 0.01);
}

#[test]
fn revisiting_an_unreadable_snapshot_does_not_block_other_visits() {
    let (_dir, db) = temp_db();
    db.get_connection()
        .execute_batch(
            "INSERT INTO directory_snapshot
        (path, score, last_accessed, access_count, half_life) VALUES
        ('/bad', 'invalid', 1700000000, 1, 43200)",
        )
        .unwrap();
    let mut history = FrecencyHistory::from_db(db.clone()).unwrap();
    assert!(item(&history, "/bad").is_none());
    history.add("/good");
    history.write_history("/bad").unwrap();
    history.reload().unwrap();
    for path in ["/bad", "/good"] {
        assert_eq!(item(&history, path).unwrap().num_accesses(), 1);
    }
}

#[test]
fn visiting_a_saturated_legacy_row_preserves_count_and_updates_context() {
    let (_dir, db) = temp_db();
    db.get_connection()
        .execute_batch(
            "INSERT INTO directory_snapshot
        (path, score, last_accessed, access_count, half_life, context)
        VALUES ('/full', 1.0, 1, 2147483647, 43200, 'old-context')",
        )
        .unwrap();
    let mut history = FrecencyHistory::from_db(db).unwrap();
    history.add("/full");
    let expected_context = item(&history, "/full").unwrap().context.clone();
    // Persist later than the visit. A legacy relative last_accessed has no epoch.
    if let DirectoryChange::Visit(visit) = &mut history.pending.lock()[0] {
        visit.at -= 60.0;
    }
    history.save().unwrap();
    history.reload().unwrap();
    let saved = item(&history, "/full").unwrap();
    assert_eq!(saved.num_accesses(), i32::MAX);
    assert_eq!(saved.context, expected_context);
}
