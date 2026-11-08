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
    let missing = dir.path().join("missing").display().to_string();
    let mut history = FrecencyHistory::from_db(db.clone()).unwrap();
    history.add(&kept);
    history.add(&missing);
    history.save().unwrap();
    assert_eq!(snapshot_paths(&db), [kept.as_str(), missing.as_str()]);

    history.prune();
    history.save().unwrap();
    assert_eq!(snapshot_paths(&db), [kept.as_str()]);

    history.reload().unwrap();
    assert!(item(&history, &missing).is_none());
    let reloaded = FrecencyHistory::from_db(db.clone()).unwrap();
    assert!(item(&reloaded, &missing).is_none());
    assert!(item(&reloaded, &kept).is_some());
}

#[test]
fn directory_visits_keep_only_the_latest_rows() {
    let (_dir, db) = temp_db();
    {
        let conn = db.get_connection();
        for ts in 0..5 {
            log_visit(&conn, "/frecency/visit", ts, None, 3);
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
