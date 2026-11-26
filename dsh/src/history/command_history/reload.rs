//! Loading `History` from SQLite and refreshing it later: the initial `load`, a background-thread `request_reload`/synchronous `reload` pair that never overwrites newer local writes (revision-gated), and
//! `load_older_than` for the picker's "load more" scroll.
use super::*;
use rusqlite::{OptionalExtension, Row, types::FromSql};

fn history_entry(row: &Row<'_>) -> rusqlite::Result<Entry> {
    decode_entry::<false>(row)
}

fn decode_entry<const STRICT: bool>(row: &Row<'_>) -> rusqlite::Result<Entry> {
    fn optional<T: FromSql, const STRICT: bool>(
        row: &Row<'_>,
        index: usize,
    ) -> rusqlite::Result<Option<T>> {
        if STRICT {
            row.get(index)
        } else {
            Ok(row.get(index).ok())
        }
    }

    Ok(Entry {
        entry: row.get(0)?,
        when: row.get(1)?,
        count: optional::<_, STRICT>(row, 2)?.unwrap_or(1),
        context: optional::<_, STRICT>(row, 3)?,
        exit_code: optional::<_, STRICT>(row, 4)?,
        duration_ms: row.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
        cwd: optional::<_, STRICT>(row, 6)?,
        session_id: optional::<_, STRICT>(row, 7)?,
        hostname: optional::<_, STRICT>(row, 8)?,
    })
}

impl History {
    /// Search the saved aggregate from newest to oldest, applying the result
    /// limit after matching. Older files get the same defaults as a migration.
    pub(crate) fn search_saved_entries(&self, query: &HistoryQuery) -> Result<Vec<Entry>> {
        let Some(db) = &self.db else {
            return Ok(Vec::new());
        };
        if query.limit == Some(0) {
            return Ok(Vec::new());
        }
        let conn = db.get_connection();
        let schema = conn.prepare("SELECT * FROM command_history LIMIT 0")?;
        let columns: Vec<_> = schema.column_names().collect();
        let optional_fields = [
            ("count", "1"),
            ("context", "NULL"),
            ("exit_code", "NULL"),
            ("duration_ms", "NULL"),
            ("cwd", "NULL"),
            ("session_id", "NULL"),
            ("hostname", "NULL"),
        ]
        .map(|(column, fallback)| {
            if columns.contains(&column) {
                column
            } else {
                fallback
            }
        })
        .join(", ");
        let filter = history_command_filter(columns.contains(&"is_command"));
        let mut stmt = conn.prepare(&format!(
            "SELECT command, timestamp, {optional_fields} FROM command_history
             WHERE {filter} ORDER BY timestamp DESC, id DESC"
        ))?;
        let rows = stmt.query_map([], decode_entry::<true>)?;
        let matcher = EntryMatcher::new(query);
        let mut matched = Vec::new();
        for row in rows {
            let entry = row?;
            if matcher.matches(&entry, None) {
                matched.push(entry);
                if query.limit.is_some_and(|limit| matched.len() >= limit) {
                    break;
                }
            }
        }
        Ok(matched)
    }

    /// Load all history entries.
    pub fn load(&mut self) -> Result<usize> {
        self.load_recent(10000).map(|_| self.histories.len())
    }

    /// Load recent history entries up to the given limit.
    pub fn load_recent(&mut self, limit: usize) -> Result<i64> {
        let mut min_timestamp = 0;
        if let Some(db) = &self.db {
            let conn = db.get_connection();
            let mut stmt = conn.prepare(
                "SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                 FROM (
                    SELECT id, command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                    FROM command_history
                    WHERE COALESCE(is_command, dogesh_history_command(command))
                    ORDER BY timestamp DESC, id DESC
                    LIMIT ?1
                 ) 
                 ORDER BY timestamp ASC, id ASC",
            )?;

            let rows = stmt.query_map([limit as i64], history_entry)?;

            self.histories.clear();

            for r in rows.flatten() {
                self.histories.push(r);
            }

            if let Some(first) = self.histories.first() {
                min_timestamp = first.when;
            }

            self.current_index = self.histories.len();

            self.recent_cache = Self::recent_commands(&self.histories);
        }
        self.rebuild_normalized_entries();
        self.bump_revision();
        Ok(min_timestamp)
    }

    /// The live window may omit older aggregate rows. Fetch only missing
    /// commands named by the import so their latest metadata and counts are
    /// available before applying the batch, including for memory-side queries.
    pub(super) fn read_import_baseline(
        &self,
        tx: &rusqlite::Transaction<'_>,
        entries: &[(String, i64)],
    ) -> Result<Vec<Entry>> {
        if !tx.query_row("SELECT EXISTS(SELECT 1 FROM command_history)", [], |row| {
            row.get::<_, bool>(0)
        })? {
            return Ok(Vec::new());
        }
        let mut missing: std::collections::HashSet<_> = entries
            .iter()
            .map(|(command, _)| command.as_str())
            .collect();
        for entry in &self.histories {
            missing.remove(entry.entry.as_str());
        }
        let mut stored = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname, id
                 FROM command_history WHERE command = ?1",
            )?;
            for command in missing {
                if let Some(row) = stmt
                    .query_row([command], |row| {
                        Ok((row.get::<_, i64>(9)?, history_entry(row)?))
                    })
                    .optional()?
                {
                    stored.push(row);
                }
            }
        }
        // Missing rows from a limited load precede resident rows in a
        // timestamp tie, as load_recent selects the highest ids first.
        stored.sort_unstable_by_key(|(id, _)| *id);
        Ok(stored.into_iter().map(|(_, entry)| entry).collect())
    }

    /// Load entries older than the given timestamp.
    pub fn load_older_than(&self, timestamp: i64, limit: usize) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        if let Some(db) = &self.db {
            let conn = db.get_connection();
            let mut stmt = conn.prepare(
                "SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                 FROM (
                    SELECT id, command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                    FROM command_history 
                    WHERE timestamp < ?1 AND COALESCE(is_command, dogesh_history_command(command))
                    ORDER BY timestamp DESC, id DESC
                    LIMIT ?2
                 ) 
                 ORDER BY timestamp ASC, id ASC",
            )?;

            let rows = stmt.query_map(rusqlite::params![timestamp, limit as i64], history_entry)?;

            for r in rows.flatten() {
                entries.push(r);
            }
        }
        Ok(entries)
    }

    /// Prepend entries to the beginning of history.
    pub fn prepend(&mut self, mut entries: Vec<Entry>) {
        entries.append(&mut self.histories);
        self.histories = entries;
        self.rebuild_normalized_entries();
        self.reset_index();
        self.bump_revision();
    }

    /// Reload history from the database.
    pub fn reload(&mut self) -> Result<()> {
        let Some(db) = self.db.clone() else {
            return Ok(());
        };

        // Only reload if we are not in the middle of navigation (at end of history)
        if !self.at_end() {
            return Ok(());
        }

        let snapshot = if self.sender.is_some() {
            // Use the same FIFO barrier as an asynchronous reload. All earlier
            // batches then have a definite SQLite or memory-only outcome.
            let (tx, rx) = mpsc::channel();
            if !self.request_reload(move |snapshot| {
                let _ = tx.send(snapshot);
            }) {
                anyhow::bail!("command history writer disconnected");
            }
            rx.recv()??
        } else {
            Self::load_reload_snapshot(
                &db,
                self.revision,
                self.pending_entries_snapshot(),
                &self.failed_occurrences,
            )?
        };
        let _ = self.apply_reload_snapshot(snapshot);
        Ok(())
    }

    pub(crate) fn request_reload<F>(&mut self, complete: F) -> bool
    where
        F: FnOnce(Result<CommandHistoryReloadSnapshot>) + Send + 'static,
    {
        if !self.at_end() {
            return false;
        }
        self.drain_persistence_acks();
        let Some(sender) = &self.sender else {
            return false;
        };
        let pending_entries = self.pending_entries_snapshot();
        sender
            .send(HistoryMsg::Reload {
                base_revision: self.revision,
                pending_entries,
                complete: Box::new(complete),
            })
            .is_ok()
    }
    pub(super) fn load_reload_snapshot(
        db: &Db,
        base_revision: u64,
        pending_entries: Vec<PendingHistoryEntry>,
        failed_occurrences: &HashMap<String, FailedOccurrences>,
    ) -> Result<CommandHistoryReloadSnapshot> {
        let mut conn = db.get_connection();
        let tx = conn.transaction()?;
        let mut stmt = tx.prepare(
            "SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                 FROM (
                    SELECT id, command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                    FROM command_history
                    WHERE COALESCE(is_command, dogesh_history_command(command))
                    ORDER BY timestamp DESC, id DESC
                    LIMIT 10000
                 ) 
                 ORDER BY timestamp ASC, id ASC",
        )?;

        let rows = stmt.query_map([], history_entry)?;

        let mut stored = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        if !failed_occurrences.is_empty() {
            // An older pending row may be outside the recent query. Read its
            // actual baseline in the same transaction before adding the delta.
            let resident: std::collections::HashSet<_> =
                stored.iter().map(|e| e.entry.as_str()).collect();
            let mut missing = Vec::new();
            let mut older = tx.prepare("SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname, id FROM command_history WHERE command=?1")?;
            for command in failed_occurrences
                .keys()
                .filter(|c| !resident.contains(c.as_str()))
            {
                if let Some(row) = older
                    .query_row([command], |row| {
                        Ok((row.get::<_, i64>(9)?, history_entry(row)?))
                    })
                    .optional()?
                {
                    missing.push(row);
                }
            }
            missing.sort_unstable_by_key(|(id, e)| (e.when, *id));
            stored.splice(0..0, missing.into_iter().map(|(_, e)| e));
        }
        tx.commit()?;
        drop(conn);
        let histories = super::merge::merge_entries(
            stored,
            pending_entries
                .into_iter()
                .enumerate()
                .map(|(index, pending)| {
                    let failed = failed_occurrences.get(&pending.entry.entry);
                    // A historical failure changes the count only. A new
                    // successful write already has an authoritative SQLite id.
                    let advance_tie = failed.is_some_and(|f| {
                        Some(f.latest) == pending.last_occurrence
                            && f.latest.0 == pending.entry.when
                    });
                    (
                        index,
                        pending.entry,
                        super::merge::CountMerge::Pending {
                            failed: failed.map_or(0, |f| f.count),
                            advance_tie,
                        },
                    )
                }),
        );
        let normalized_entries = histories
            .iter()
            .map(|entry| Self::normalized_command(&entry.entry))
            .collect();
        let recent_cache = Self::recent_commands(&histories);

        Ok(CommandHistoryReloadSnapshot {
            base_revision,
            histories,
            normalized_entries,
            recent_cache,
        })
    }

    pub(crate) fn apply_reload_snapshot(
        &mut self,
        snapshot: CommandHistoryReloadSnapshot,
    ) -> HistoryReloadApply {
        if !self.at_end() {
            return HistoryReloadApply::Navigating;
        }
        if self.revision != snapshot.base_revision {
            return HistoryReloadApply::Stale;
        }

        self.histories = snapshot.histories;
        self.normalized_entries = snapshot.normalized_entries;
        self.recent_cache = snapshot.recent_cache;
        self.reset_index();
        self.bump_revision();
        HistoryReloadApply::Applied
    }

    pub(crate) fn reload_snapshot_for_probe(
        &self,
        target: &History,
    ) -> CommandHistoryReloadSnapshot {
        CommandHistoryReloadSnapshot {
            base_revision: target.revision,
            histories: self.histories.clone(),
            normalized_entries: self.normalized_entries.clone(),
            recent_cache: self.recent_cache.clone(),
        }
    }

    pub(super) fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}
