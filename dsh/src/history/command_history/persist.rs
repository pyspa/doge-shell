//! Writing to SQLite: the background writer thread (`start_background_writer`), its synchronous counterparts (`write_batch_sync`, `record_outcome_sync`), and the pending-write ledger that reconciles a background reload with
//! writes still in flight (`track_pending_entries`/`acknowledge_persistence`).
use super::*;

impl History {
    fn next_persistence_id(&mut self) -> u64 {
        self.next_persistence_id = self.next_persistence_id.wrapping_add(1);
        self.next_persistence_id
    }

    fn track_pending_entries(&mut self, persistence_id: u64, commands: &[String]) {
        for command in commands {
            if let Some(entry) = self
                .histories
                .iter()
                .rev()
                .find(|entry| entry.entry == *command)
                .cloned()
            {
                self.pending_persistence.insert(
                    command.clone(),
                    PendingHistoryEntry {
                        persistence_id,
                        entry,
                    },
                );
            }
        }
    }

    fn acknowledge_persistence(&mut self, persistence_id: u64, commands: &[String]) {
        for command in commands {
            if self
                .pending_persistence
                .get(command)
                .is_some_and(|pending| pending.persistence_id == persistence_id)
            {
                self.pending_persistence.remove(command);
            }
        }
    }

    pub(super) fn drain_persistence_acks(&mut self) {
        let acks = self
            .persist_ack_rx
            .as_ref()
            .map(|receiver| receiver.lock().try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for ack in acks {
            match ack.result {
                Ok(()) => self.acknowledge_persistence(ack.persistence_id, &ack.commands),
                Err(error) => {
                    tracing::warn!("background command history write failed: {error}");
                }
            }
        }
    }

    pub(super) fn pending_entries_snapshot(&self) -> Vec<Entry> {
        self.pending_persistence
            .values()
            .map(|pending| pending.entry.clone())
            .collect()
    }
    /// Start the background writer thread.
    pub fn start_background_writer(&mut self) {
        if let Some(db_clone) = self.db.clone() {
            let (tx, rx) = mpsc::channel();
            let (ack_tx, ack_rx) = mpsc::channel();
            self.sender = Some(tx);
            self.persist_ack_rx = Some(Arc::new(ParkingMutex::new(ack_rx)));

            thread::spawn(move || {
                let mut db = db_clone;
                while let Ok(msg) = rx.recv() {
                    match msg {
                        HistoryMsg::WriteBatch {
                            entries,
                            context,
                            persistence_id,
                            commands,
                        } => {
                            let result = Self::write_batch_sync(&mut db, entries, context)
                                .map_err(|error| error.to_string());
                            let _ = ack_tx.send(HistoryPersistAck {
                                persistence_id,
                                commands,
                                result,
                            });
                        }
                        HistoryMsg::RecordOutcome {
                            command,
                            context,
                            metadata,
                            persistence_id,
                        } => {
                            let result =
                                Self::record_outcome_sync(&mut db, &command, context, &metadata)
                                    .map_err(|error| error.to_string());
                            let _ = ack_tx.send(HistoryPersistAck {
                                persistence_id,
                                commands: vec![command],
                                result,
                            });
                        }
                        HistoryMsg::Reload {
                            base_revision,
                            pending_entries,
                            complete,
                        } => {
                            complete(Self::load_reload_snapshot(
                                &db,
                                base_revision,
                                pending_entries,
                            ));
                        }
                    }
                }
            });
        }
    }

    /// Synchronously write a batch of entries to the database.
    fn write_batch_sync(
        db: &mut Db,
        entries: Vec<(String, i64)>,
        context: Option<String>,
    ) -> Result<()> {
        let mut conn = db.get_connection();
        let tx = conn.transaction()?;

        {
            let mut upsert_stmt = tx.prepare(
                "INSERT INTO command_history (command, timestamp, context, count) 
                  VALUES (?1, ?2, ?3, 1)
                  ON CONFLICT(command) DO UPDATE SET 
                      count = count + 1,
                      timestamp = excluded.timestamp,
                      context = excluded.context
                  RETURNING count",
            )?;

            for (cmd, when) in &entries {
                let _count: i64 = upsert_stmt
                    .query_row(rusqlite::params![cmd, when, context], |row| row.get(0))
                    .unwrap_or(1);
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn update_metadata_sync(
        db: &mut Db,
        command: &str,
        context: Option<String>,
        metadata: &HistoryMetadata,
    ) -> Result<()> {
        let conn = db.get_connection();
        conn.execute(
            "UPDATE command_history
             SET context = COALESCE(?2, context),
                 exit_code = ?3,
                 duration_ms = ?4,
                 cwd = ?5,
                 session_id = ?6,
                 hostname = ?7
             WHERE command = ?1",
            rusqlite::params![
                command,
                context,
                metadata.exit_code,
                metadata.duration_ms.map(|v| v as i64),
                metadata.cwd,
                metadata.session_id,
                metadata.hostname
            ],
        )?;
        Ok(())
    }

    pub(super) fn record_outcome_sync(
        db: &mut Db,
        command: &str,
        context: Option<String>,
        metadata: &HistoryMetadata,
    ) -> Result<()> {
        Self::update_metadata_sync(db, command, context, metadata)?;
        if metadata.ledger_mode == CommandLedgerMode::Off {
            return Ok(());
        }
        let output = (metadata.ledger_mode == CommandLedgerMode::Output)
            .then(|| metadata.output.as_deref().map(truncate_ledger_output))
            .flatten();
        let conn = db.get_connection();
        conn.execute(
            "INSERT INTO command_events
             (command, cwd, started_at, duration_ms, exit_code, session_id, hostname, author, output)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                command,
                metadata.cwd,
                metadata.started_at,
                metadata.duration_ms.map(|value| value as i64),
                metadata.exit_code,
                metadata.session_id,
                metadata.hostname,
                metadata.author,
                output,
            ],
        )?;
        conn.execute(
            "DELETE FROM command_events WHERE started_at < ?1",
            [chrono::Utc::now().timestamp() - LEDGER_RETENTION_SECONDS],
        )?;
        conn.execute(
            "DELETE FROM command_events
             WHERE id NOT IN (
                 SELECT id FROM command_events
                 ORDER BY started_at DESC, id DESC LIMIT ?1
             )",
            [LEDGER_MAX_EVENTS],
        )?;
        enqueue_atuin_dual_write(CommandEvent {
            id: 0,
            command: command.to_string(),
            cwd: metadata.cwd.clone(),
            started_at: metadata.started_at,
            duration_ms: metadata.duration_ms,
            exit_code: metadata.exit_code,
            session_id: metadata.session_id.clone(),
            hostname: metadata.hostname.clone(),
            author: metadata.author.clone(),
            output: None,
        });
        Ok(())
    }

    // Lifecycle hooks kept for API symmetry with backing stores that
    // require explicit open/close; currently no-ops.
    #[allow(dead_code)]
    pub(crate) fn open(&mut self) -> Result<&mut History> {
        Ok(self)
    }

    #[allow(dead_code)]
    pub(crate) fn close(&mut self) -> Result<()> {
        Ok(())
    }

    /// Write a single history entry.
    pub fn write_history(&mut self, history: &str) -> Result<()> {
        self.write_batch(vec![(history.to_string(), Local::now().timestamp())])
    }

    /// Write a batch of history entries.
    pub fn write_batch(&mut self, entries: Vec<(String, i64)>) -> Result<()> {
        self.drain_persistence_acks();
        let context = get_current_context();

        if self.normalized_entries.len() != self.histories.len() {
            self.rebuild_normalized_entries();
        }

        // 1. Update in-memory history immediately
        for (cmd, when) in &entries {
            let mut count = 1;
            if let Some(pos) = self.histories.iter().position(|e| e.entry == *cmd) {
                count = self.histories[pos].count + 1;
                self.histories.remove(pos);
                self.normalized_entries.remove(pos);
            }
            self.histories.push(Entry {
                entry: cmd.clone(),
                when: *when,
                count,
                context: context.clone(),
                exit_code: None,
                duration_ms: None,
                cwd: None,
                session_id: None,
                hostname: None,
            });
            self.normalized_entries.push(Self::normalized_command(cmd));
        }
        self.reset_index();

        // Update recent cache
        for (cmd, _) in &entries {
            self.recent_cache.retain(|e| e != cmd);
            self.recent_cache.push(cmd.clone());
            if self.recent_cache.len() > 100 {
                self.recent_cache.remove(0);
            }
        }
        if !entries.is_empty() {
            self.bump_revision();
        }

        // 2. Persist
        let persistence_id = self.next_persistence_id();
        let commands = entries
            .iter()
            .map(|(command, _)| command.clone())
            .collect::<Vec<_>>();
        self.track_pending_entries(persistence_id, &commands);
        if let Some(sender) = &self.sender {
            let _ = sender.send(HistoryMsg::WriteBatch {
                entries,
                context,
                persistence_id,
                commands,
            });
        } else if let Some(db) = &mut self.db
            && Self::write_batch_sync(db, entries, context).is_ok()
        {
            self.acknowledge_persistence(persistence_id, &commands);
        }
        Ok(())
    }

    pub fn record_outcome(&mut self, command: &str, metadata: HistoryMetadata) -> Result<()> {
        self.drain_persistence_acks();
        if let Some(entry) = self
            .histories
            .iter_mut()
            .rev()
            .find(|entry| entry.entry == command)
        {
            entry.context = get_current_context();
            entry.exit_code = metadata.exit_code;
            entry.duration_ms = metadata.duration_ms;
            entry.cwd = metadata.cwd.clone();
            entry.session_id = metadata.session_id.clone();
            entry.hostname = metadata.hostname.clone();
        }

        let context = get_current_context();
        let persistence_id = self.next_persistence_id();
        let commands = vec![command.to_string()];
        self.track_pending_entries(persistence_id, &commands);
        if let Some(sender) = &self.sender {
            let _ = sender.send(HistoryMsg::RecordOutcome {
                command: command.to_string(),
                context,
                metadata,
                persistence_id,
            });
        } else if let Some(db) = &mut self.db
            && Self::record_outcome_sync(db, command, context, &metadata).is_ok()
        {
            self.acknowledge_persistence(persistence_id, &commands);
        }
        self.bump_revision();
        Ok(())
    }
}
