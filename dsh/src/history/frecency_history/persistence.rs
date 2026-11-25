//! Persist directory visits against the latest committed row for each path.
use super::*;
use parking_lot::Mutex;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

pub(super) enum DirectoryChange {
    Visit {
        path: String,
        at: f64,
        context: Option<String>,
        half_life: f32,
    },
    Delete(Vec<String>),
}
pub(super) type PendingChanges = Arc<Mutex<Vec<DirectoryChange>>>;

/// Rows of `directory_visits` kept after each insert. Nothing reads the log
/// yet, so it only has to stay bounded.
const MAX_DIRECTORY_VISITS: i64 = 10_000;

/// `last_accessed` is saved as absolute epoch seconds. Builds before that
/// saved a session-relative offset, which cannot be restored; such small
/// values are left at the load time, as before.
const MIN_ABSOLUTE_ACCESS_TIME: f64 = 1_000_000_000.0;

const SNAPSHOT_SELECT: &str = "SELECT path, score, last_accessed, access_count, half_life, context, score_time FROM directory_snapshot";

/// Take the connection first so background, synchronous and retry flushes
/// commit in order. The foreground queue stays unlocked throughout disk I/O.
pub(super) fn flush_changes(db: &Db, pending: &PendingChanges) -> Result<()> {
    let mut conn = db.get_connection();
    let mut changes = std::mem::take(&mut *pending.lock());
    if changes.is_empty() {
        return Ok(());
    }
    let result = write_changes(&mut conn, &changes);
    if result.is_err() {
        let mut queue = pending.lock();
        changes.append(&mut queue);
        *queue = changes;
    }
    result
}

fn write_changes(conn: &mut rusqlite::Connection, changes: &[DirectoryChange]) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let store = FrecencyStore::default();
    let mut select = tx.prepare(&format!("{SNAPSHOT_SELECT} WHERE path = ?1"))?;
    for change in changes {
        match change {
            DirectoryChange::Visit {
                path,
                at,
                context,
                half_life,
            } => {
                let previous = select
                    .query_row([path], |row| {
                        let last: f64 = row.get(2)?;
                        let absolute_last =
                            (last.is_finite() && last >= MIN_ABSOLUTE_ACCESS_TIME).then_some(last);
                        decode_item(row, &store).map(|item| (item, absolute_last))
                    })
                    .optional();
                let previous = match previous {
                    Err(err) if invalid_snapshot_value(&err) => {
                        tracing::warn!(
                            "frecency: rebuilding unreadable snapshot row for {path}: {err}"
                        );
                        None
                    }
                    other => other?,
                };
                let now = dsh_frecency::current_time_secs();
                let mut count = 0;
                let mut score = 0.0;
                let mut last = *at;
                let mut latest_context = context.as_deref();
                if let Some((item, previous_last)) = &previous {
                    count = item.num_accesses();
                    score = item.get_frecency();
                    last = last.max(previous_last.unwrap_or(*at));
                    if latest_context.is_none() || *at < last {
                        latest_context = item.context.as_deref();
                    }
                }
                let weight = (-((now - at).max(0.0) as f32) / half_life).exp2();
                tx.execute(
                    "INSERT OR REPLACE INTO directory_snapshot
                        (path, score, last_accessed, access_count, half_life, context, score_time)
                        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        path,
                        score + weight,
                        last,
                        count.saturating_add(1),
                        half_life,
                        latest_context,
                        now
                    ],
                )?;
                log_visit(
                    &tx,
                    path,
                    *at as i64,
                    context.as_deref(),
                    MAX_DIRECTORY_VISITS,
                )?;
            }
            DirectoryChange::Delete(paths) => {
                for path in paths {
                    tx.execute("DELETE FROM directory_snapshot WHERE path = ?1", [path])?;
                }
            }
        }
    }
    drop(select);
    tx.commit()?;
    Ok(())
}

/// Read every snapshot row as an item relative to `store`'s reference time.
/// A row that cannot be decoded is skipped rather than failing the load.
pub(super) fn read_snapshot_items(
    conn: &rusqlite::Connection,
    store: &FrecencyStore,
) -> rusqlite::Result<Vec<ItemStats>> {
    let mut stmt = conn.prepare(SNAPSHOT_SELECT)?;
    let rows = stmt.query_map([], |row| decode_item(row, store))?;

    let mut items = Vec::new();
    for row in rows {
        match row {
            Ok(item) => items.push(item),
            Err(err) if invalid_snapshot_value(&err) => {
                tracing::warn!("frecency: skipping unreadable snapshot row: {err}")
            }
            Err(err) => return Err(err),
        }
    }
    Ok(items)
}

// A malformed cache row can be rebuilt on its next visit. Operational
// SQLite errors must still roll back the batch and preserve the retry queue.
fn invalid_snapshot_value(err: &rusqlite::Error) -> bool {
    use rusqlite::Error::*;
    matches!(
        err,
        InvalidColumnType(..)
            | FromSqlConversionFailure(..)
            | IntegralValueOutOfRange(..)
            | Utf8Error(..)
    )
}

fn decode_item(row: &rusqlite::Row<'_>, store: &FrecencyStore) -> rusqlite::Result<ItemStats> {
    // `last_accessed` is REAL in practice despite the INTEGER column
    // affinity; reading it as f64 accepts both storage classes.
    let path: String = row.get(0)?;
    let score: f64 = row.get(1)?;
    let last_accessed: f64 = row.get(2)?;
    let access_count: i64 = row.get(3)?;
    let half_life: f64 = row.get(4)?;
    let context: Option<String> = row.get(5)?;

    let half_life = if half_life.is_finite() && half_life > 0.0 {
        half_life as f32
    } else {
        store.half_life
    };
    let access_count = i32::try_from(access_count.max(0)).unwrap_or(i32::MAX);
    let mut item = ItemStats::new(&path, store.reference_time, half_life, context);
    item.update_num_accesses(access_count);
    let current_format = last_accessed.is_finite() && last_accessed >= MIN_ABSOLUTE_ACCESS_TIME;
    if current_format {
        item.update_last_access(last_accessed);
    }
    let score = finite_score(score as f32, access_count);
    // A legacy row's score is inflated by 2^(session age / half-life).
    // A current score never exceeds the access count (each access adds
    // at most 1 and then decays), so cap it there instead of letting old
    // directories outrank every fresh visit until they decay.
    let score = if current_format {
        score
    } else {
        score.min(access_count.max(1) as f32)
    };
    let score_time: Option<f64> = row.get(6)?;
    let elapsed = score_time.map_or(0.0, |at| (dsh_frecency::current_time_secs() - at).max(0.0));
    item.set_frecency(score * (-elapsed as f32 / half_life).exp2());
    Ok(item)
}

/// Older builds saved the time-scaled internal score, which grew on every
/// restart until it overflowed. Fall back to the access count for a score
/// that is not a finite, non-negative number.
fn finite_score(score: f32, access_count: i32) -> f32 {
    if score.is_finite() && score >= 0.0 {
        score
    } else {
        access_count.max(1) as f32
    }
}

/// Append a visit and drop all but the latest `keep` rows. Ids are
/// AUTOINCREMENT, so they only grow and the cut-off is a primary-key range.
pub(super) fn log_visit(
    conn: &rusqlite::Connection,
    path: &str,
    timestamp: i64,
    context: Option<&str>,
    keep: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO directory_visits (path, timestamp, context) VALUES (?1, ?2, ?3)",
        rusqlite::params![path, timestamp, context],
    )?;
    conn.execute(
        "DELETE FROM directory_visits WHERE id <= last_insert_rowid() - ?1",
        [keep],
    )?;
    Ok(())
}
