//! Loading `History` from SQLite and refreshing it later: the initial `load`, a background-thread `request_reload`/synchronous `reload` pair that never overwrites newer local writes (revision-gated), and
//! `load_older_than` for the picker's "load more" scroll.
use super::*;

impl History {
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
                    SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                    FROM command_history 
                    ORDER BY timestamp DESC 
                    LIMIT ?1
                 ) 
                 ORDER BY timestamp ASC",
            )?;

            let rows = stmt.query_map([limit as i64], |row| {
                Ok(Entry {
                    entry: row.get(0)?,
                    when: row.get(1)?,
                    count: row.get(2).unwrap_or(1),
                    context: row.get(3).ok(),
                    exit_code: row.get(4).ok(),
                    duration_ms: row.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
                    cwd: row.get(6).ok(),
                    session_id: row.get(7).ok(),
                    hostname: row.get(8).ok(),
                })
            })?;

            self.histories.clear();

            for r in rows.flatten() {
                self.histories.push(r);
            }

            if let Some(first) = self.histories.first() {
                min_timestamp = first.when;
            }

            self.current_index = self.histories.len();

            // Initialize recent cache from loaded history (last 100 entries)
            self.recent_cache.clear();
            for entry in self.histories.iter().rev().take(100) {
                self.recent_cache.insert(0, entry.entry.clone());
            }
        }
        self.rebuild_normalized_entries();
        self.bump_revision();
        Ok(min_timestamp)
    }

    /// Load entries older than the given timestamp.
    pub fn load_older_than(&self, timestamp: i64, limit: usize) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        if let Some(db) = &self.db {
            let conn = db.get_connection();
            let mut stmt = conn.prepare(
                "SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                 FROM (
                    SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                    FROM command_history 
                    WHERE timestamp < ?1
                    ORDER BY timestamp DESC 
                    LIMIT ?2
                 ) 
                 ORDER BY timestamp ASC",
            )?;

            let rows = stmt.query_map(rusqlite::params![timestamp, limit as i64], |row| {
                Ok(Entry {
                    entry: row.get(0)?,
                    when: row.get(1)?,
                    count: row.get(2).unwrap_or(1),
                    context: row.get(3).ok(),
                    exit_code: row.get(4).ok(),
                    duration_ms: row.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
                    cwd: row.get(6).ok(),
                    session_id: row.get(7).ok(),
                    hostname: row.get(8).ok(),
                })
            })?;

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

        self.drain_persistence_acks();
        let snapshot =
            Self::load_reload_snapshot(&db, self.revision, self.pending_entries_snapshot())?;
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
        pending_entries: Vec<Entry>,
    ) -> Result<CommandHistoryReloadSnapshot> {
        let conn = db.get_connection();
        let mut stmt = conn.prepare(
            "SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                 FROM (
                    SELECT command, timestamp, count, context, exit_code, duration_ms, cwd, session_id, hostname
                    FROM command_history 
                    ORDER BY timestamp DESC 
                    LIMIT 10000
                 ) 
                 ORDER BY timestamp ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(Entry {
                entry: row.get(0)?,
                when: row.get(1)?,
                count: row.get(2).unwrap_or(1),
                context: row.get(3).ok(),
                exit_code: row.get(4).ok(),
                duration_ms: row.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
                cwd: row.get(6).ok(),
                session_id: row.get(7).ok(),
                hostname: row.get(8).ok(),
            })
        })?;

        let mut histories = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Self::merge_pending_entries(&mut histories, pending_entries);
        let normalized_entries = histories
            .iter()
            .map(|entry| Self::normalized_command(&entry.entry))
            .collect();
        let mut recent_cache = histories
            .iter()
            .rev()
            .take(100)
            .map(|entry| entry.entry.clone())
            .collect::<Vec<_>>();
        recent_cache.reverse();

        Ok(CommandHistoryReloadSnapshot {
            base_revision,
            histories,
            normalized_entries,
            recent_cache,
        })
    }

    fn merge_pending_entries(histories: &mut Vec<Entry>, pending_entries: Vec<Entry>) {
        for local in pending_entries {
            match histories
                .iter()
                .position(|entry| entry.entry == local.entry)
            {
                Some(index) if histories[index].when <= local.when => {
                    let db_entry = &mut histories[index];
                    db_entry.when = local.when;
                    db_entry.count = db_entry.count.max(local.count);
                    db_entry.context = local.context.or_else(|| db_entry.context.clone());
                    db_entry.exit_code = local.exit_code.or(db_entry.exit_code);
                    db_entry.duration_ms = local.duration_ms.or(db_entry.duration_ms);
                    db_entry.cwd = local.cwd.or_else(|| db_entry.cwd.clone());
                    db_entry.session_id = local.session_id.or_else(|| db_entry.session_id.clone());
                    db_entry.hostname = local.hostname.or_else(|| db_entry.hostname.clone());
                }
                Some(_) => {}
                None => histories.push(local),
            }
        }
        histories.sort_by(|left, right| {
            left.when
                .cmp(&right.when)
                .then(left.entry.cmp(&right.entry))
        });
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
