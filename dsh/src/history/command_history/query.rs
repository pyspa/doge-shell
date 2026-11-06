//! Read-only queries over `History`: prefix search, the `HistoryQuery`/`EntryMatcher`-driven `search_entries`, event log reads for `shell_history`, and the test-only entry injector.
use super::*;

impl History {
    pub fn command_events(&self, author: Option<&str>, limit: usize) -> Result<Vec<CommandEvent>> {
        self.command_events_filtered(author, limit, false)
    }

    pub fn command_events_filtered(
        &self,
        author: Option<&str>,
        limit: usize,
        failures_only: bool,
    ) -> Result<Vec<CommandEvent>> {
        self.search_command_events(
            author,
            &HistoryQuery {
                status: if failures_only {
                    HistoryStatusFilter::Failure
                } else {
                    HistoryStatusFilter::Any
                },
                limit: Some(limit),
                ..Default::default()
            },
        )
    }

    /// Read ledger events newest first, applying every condition before the limit.
    /// Project context is inferred from the current filesystem, since the ledger
    /// stores cwd but no historical project marker. Cache each distinct cwd.
    pub fn search_command_events(
        &self,
        author: Option<&str>,
        query: &HistoryQuery,
    ) -> Result<Vec<CommandEvent>> {
        let Some(db) = &self.db else {
            return Ok(Vec::new());
        };
        if query.limit == Some(0) {
            return Ok(Vec::new());
        }
        let conn = db.get_connection();
        let author = author.filter(|author| *author != "all");
        let sql = if author.is_some() {
            "SELECT id, command, cwd, started_at, duration_ms, exit_code, session_id, hostname, author, output
             FROM command_events WHERE author = ?1 ORDER BY started_at DESC, id DESC"
        } else {
            "SELECT id, command, cwd, started_at, duration_ms, exit_code, session_id, hostname, author, output
             FROM command_events ORDER BY started_at DESC, id DESC"
        };
        let mut stmt = conn.prepare(sql)?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok(CommandEvent {
                id: row.get(0)?,
                command: row.get(1)?,
                cwd: row.get(2)?,
                started_at: row.get(3)?,
                duration_ms: row
                    .get::<_, Option<i64>>(4)?
                    .map(|value| value.max(0) as u64),
                exit_code: row.get(5)?,
                session_id: row.get(6)?,
                hostname: row.get(7)?,
                author: row.get(8)?,
                output: row.get(9)?,
            })
        };
        let rows = stmt.query_map(rusqlite::params_from_iter(author), map_row)?;
        let matcher = EntryMatcher::new(query);
        let mut projects = HashMap::new();
        let mut matched = Vec::new();
        // Stream rather than materializing the ledger: memory for event results
        // is bounded by the requested limit, even when older events must be read.
        for row in rows {
            let event = row?;
            let project = if query.scope == HistoryScope::Project {
                event.cwd.as_deref().and_then(|cwd| {
                    projects
                        .entry(cwd.to_owned())
                        .or_insert_with(|| {
                            super::super::context::get_current_context_from(std::path::Path::new(
                                cwd,
                            ))
                        })
                        .as_deref()
                })
            } else {
                None
            };
            if !matcher.matches_event(&event, project) {
                continue;
            }
            matched.push(event);
            if query.limit.is_some_and(|limit| matched.len() >= limit) {
                break;
            }
        }
        Ok(matched)
    }

    pub fn record_external_event(&mut self, event: CommandEvent) -> Result<()> {
        let Some(db) = &mut self.db else {
            return Ok(());
        };
        let metadata = HistoryMetadata {
            exit_code: event.exit_code,
            duration_ms: event.duration_ms,
            cwd: event.cwd,
            session_id: event.session_id,
            hostname: event.hostname,
            started_at: event.started_at,
            author: event.author,
            output: event.output,
            ledger_mode: CommandLedgerMode::Output,
        };
        Self::record_outcome_sync(db, &event.command, None, &metadata)
    }

    /// Search for the first entry matching the given prefix.
    pub fn search_first(&self, word: &str) -> Option<&str> {
        // First, check recent cache (fast path)
        for entry in self.recent_cache.iter().rev() {
            if entry.starts_with(word) {
                return Some(entry);
            }
        }
        // Fall back to full history search
        for hist in self.histories.iter().rev() {
            if hist.entry.starts_with(word) {
                return Some(&hist.entry);
            }
        }
        None
    }

    /// Get recent commands for context.
    pub fn get_recent_context(&self, limit: usize) -> Vec<String> {
        self.histories
            .iter()
            .rev()
            .take(limit)
            .map(|e| e.entry.clone())
            .collect()
    }

    pub fn search_entries(&self, query: &HistoryQuery) -> Vec<Entry> {
        if query.limit == Some(0) {
            return Vec::new();
        }

        let matcher = EntryMatcher::new(query);
        let cache_usable = self.normalized_entries.len() == self.histories.len();
        let mut matched = Vec::new();

        for (index, entry) in self.histories.iter().enumerate().rev() {
            let normalized_entry = cache_usable.then(|| self.normalized_entries[index].as_str());
            if !matcher.matches(entry, normalized_entry) {
                continue;
            }

            matched.push(entry.clone());
            if let Some(limit) = query.limit
                && matched.len() >= limit
            {
                break;
            }
        }

        matched
    }

    /// The most recent `max` entries, newest first, as an owned snapshot.
    ///
    /// Lets an interactive picker re-filter on every keystroke without holding
    /// the history lock for the duration of the session.
    pub fn snapshot_entries(&self, max: usize) -> Vec<Entry> {
        self.histories.iter().rev().take(max).cloned().collect()
    }

    /// Get an iterator over history entries.
    pub fn iter(&self) -> std::slice::Iter<'_, Entry> {
        self.histories.iter()
    }

    /// Add a test entry (for testing only).
    #[cfg(test)]
    pub fn add_test_entry(&mut self, entry: &str) {
        self.histories.push(Entry {
            entry: entry.to_string(),
            when: Local::now().timestamp(),
            count: 1,
            context: None,
            exit_code: None,
            duration_ms: None,
            cwd: None,
            session_id: None,
            hostname: None,
        });
        self.normalized_entries
            .push(Self::normalized_command(entry));
        self.size = self.histories.len();
        self.current_index = self.histories.len();
        self.bump_revision();
    }
}
