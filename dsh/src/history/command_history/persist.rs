//! Writing to SQLite: the background writer thread (`start_background_writer`), its synchronous counterparts (`write_batch_sync`, `record_outcome_sync`), and the pending-write ledger that reconciles a background reload with
//! writes still in flight (`track_pending_entry`/`acknowledge_persistence`).
use super::*;

impl History {
    pub(super) fn next_persistence_id(&mut self) -> u64 {
        self.next_persistence_id = self.next_persistence_id.wrapping_add(1);
        self.next_persistence_id
    }

    pub(super) fn track_pending_entry(
        pending: &mut HashMap<String, PendingHistoryEntry>,
        persistence_id: u64,
        entry: &Entry,
        operations: usize,
        occurrence: Option<(i64, u64, usize)>,
    ) {
        let pending = pending
            .entry(entry.entry.clone())
            .or_insert_with(|| PendingHistoryEntry {
                persistence_id,
                first_persistence_id: persistence_id,
                unconfirmed_operations: 0,
                entry: entry.clone(),
                last_occurrence: None,
            });
        pending.persistence_id = persistence_id;
        pending.entry.clone_from(entry);
        pending.last_occurrence = pending.last_occurrence.max(occurrence);
        // A later success can only confirm its own operations.
        pending.unconfirmed_operations += operations;
    }

    pub(super) fn acknowledge_persistence(&mut self, persistence_id: u64, commands: &[String]) {
        for command in commands {
            if let Some(pending) = self.pending_persistence.get_mut(command) {
                // An outcome for an absent entry was never tracked. Its ACK can
                // arrive after the first batch creates a pending entry; exclude it.
                if persistence_id.wrapping_sub(pending.first_persistence_id)
                    > pending
                        .persistence_id
                        .wrapping_sub(pending.first_persistence_id)
                {
                    continue;
                }
                // The single writer emits one ordered ACK per message; commands
                // preserves batch multiplicity. Sync writes use this same boundary.
                // Failed ACKs never reach here, so their obligations remain even
                // after newer successes, without retaining an ID list per command.
                pending.unconfirmed_operations -= 1;
                if pending.unconfirmed_operations == 0 && pending.persistence_id == persistence_id {
                    self.pending_persistence.remove(command);
                }
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

    pub(super) fn pending_entries_snapshot(&self) -> Vec<PendingHistoryEntry> {
        let mut entries: Vec<_> = self.pending_persistence.values().cloned().collect();
        entries.sort_unstable_by_key(|pending| {
            (pending.last_occurrence, pending.first_persistence_id)
        });
        entries
    }

    /// Start the background writer thread.
    pub fn start_background_writer(&mut self) {
        if let Some(db_clone) = self.db.clone() {
            let (tx, rx) = mpsc::channel();
            let (ack_tx, ack_rx) = mpsc::channel();
            self.sender = Some(tx);
            self.persist_ack_rx = Some(Arc::new(ParkingMutex::new(ack_rx)));

            let mut failed_occurrences = std::mem::take(&mut self.failed_occurrences);
            thread::spawn(move || {
                let mut db = db_clone;
                while let Ok(msg) = rx.recv() {
                    match msg {
                        HistoryMsg::Flush(complete) => {
                            let _ = complete.send(());
                        }
                        HistoryMsg::WriteBatch {
                            entries,
                            context,
                            persistence_id,
                            commands,
                        } => {
                            let result = Self::persist_occurrences(
                                &mut db,
                                entries,
                                context,
                                persistence_id,
                                &mut failed_occurrences,
                            )
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
                                &failed_occurrences,
                            ));
                        }
                    }
                }
            });
        }
    }

    fn persist_occurrences(
        db: &mut Db,
        entries: Vec<(String, i64)>,
        context: Option<String>,
        persistence_id: u64,
        failed: &mut HashMap<String, FailedOccurrences>,
    ) -> Result<()> {
        Self::write_batch_sync(db, &entries, context).inspect_err(|_| {
            for (position, (command, when)) in entries.iter().enumerate() {
                let occurrence = (*when, persistence_id, position);
                let missing = failed.entry(command.clone()).or_insert(FailedOccurrences {
                    count: 0,
                    latest: occurrence,
                });
                missing.count += 1;
                missing.latest = missing.latest.max(occurrence);
            }
        })
    }

    /// Synchronously write a batch of entries to the database.
    fn write_batch_sync(
        db: &mut Db,
        entries: &[(String, i64)],
        context: Option<String>,
    ) -> Result<()> {
        let mut conn = db.get_connection();
        let tx = conn.transaction()?;

        Self::upsert_batch(&tx, entries, context.as_deref())?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn upsert_batch(
        tx: &rusqlite::Transaction<'_>,
        entries: &[(String, i64)],
        context: Option<&str>,
    ) -> Result<()> {
        let mut upsert_stmt = tx.prepare(
                "INSERT INTO command_history (command, timestamp, context, count, is_command)
                 VALUES (?1, ?2, ?3, 1, 1)
                 ON CONFLICT(command) DO UPDATE SET
                     is_command = 1,
                     count = COALESCE(count, 1) + 1,
                     id = CASE WHEN excluded.timestamp >= timestamp THEN excluded.id ELSE id END,
                     timestamp = MAX(timestamp, excluded.timestamp),
                     context = CASE WHEN excluded.timestamp >= timestamp THEN excluded.context ELSE context END,
                     exit_code = CASE WHEN excluded.timestamp < timestamp THEN exit_code END,
                     duration_ms = CASE WHEN excluded.timestamp < timestamp THEN duration_ms END,
                     cwd = CASE WHEN excluded.timestamp < timestamp THEN cwd END,
                     session_id = CASE WHEN excluded.timestamp < timestamp THEN session_id END,
                     hostname = CASE WHEN excluded.timestamp < timestamp THEN hostname END",
            )?;

        for (cmd, when) in entries {
            upsert_stmt.execute(rusqlite::params![cmd, when, context])?;
        }
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
        Self::append_event_sync(db, command, metadata)
    }

    pub(super) fn append_event_sync(
        db: &mut Db,
        command: &str,
        metadata: &HistoryMetadata,
    ) -> Result<()> {
        if metadata.ledger_mode == CommandLedgerMode::Off {
            return Ok(());
        }
        let output = (metadata.ledger_mode == CommandLedgerMode::Output)
            .then(|| metadata.output.as_deref().map(truncate_ledger_output))
            .flatten();
        let conn = db.get_connection();
        conn.execute(
            "INSERT INTO command_events
             (command, cwd, started_at, duration_ms, exit_code, session_id, hostname, author, output, is_command)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)",
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

    /// Write a submitted dogesh command.
    pub fn write_history(&mut self, history: &str) -> Result<()> {
        if !crate::history::is_command_history_input(history) {
            return Ok(());
        }
        self.write_confirmed_command(history)
    }

    pub(crate) fn write_confirmed_command(&mut self, command: &str) -> Result<()> {
        self.write_batch(vec![(command.to_string(), Local::now().timestamp())])
    }

    /// Write entries already identified as commands by their source shell.
    pub fn write_batch(&mut self, entries: Vec<(String, i64)>) -> Result<()> {
        self.drain_persistence_acks();
        let context = get_current_context();

        let persistence_id = self.next_persistence_id();
        self.apply_batch(&entries, &context, persistence_id);
        self.reset_index();
        if !entries.is_empty() {
            self.bump_revision();
        }

        let commands = entries
            .iter()
            .map(|(command, _)| command.clone())
            .collect::<Vec<_>>();
        if let Some(sender) = &self.sender {
            sender
                .send(HistoryMsg::WriteBatch {
                    entries,
                    context,
                    persistence_id,
                    commands,
                })
                .map_err(|_| anyhow::anyhow!("command history writer disconnected"))?;
        } else if let Some(db) = &mut self.db {
            Self::persist_occurrences(
                db,
                entries,
                context,
                persistence_id,
                &mut self.failed_occurrences,
            )?;
            self.acknowledge_persistence(persistence_id, &commands);
        } else if let Some(writes) = &mut self.startup_writes {
            writes.push(StartupHistoryWrite::Batch {
                entries,
                context,
                persistence_id,
                commands,
            });
        }
        Ok(())
    }

    pub fn record_outcome(&mut self, command: &str, metadata: HistoryMetadata) -> Result<()> {
        if !crate::history::is_command_history_input(command) {
            return Ok(());
        }
        self.record_confirmed_outcome(command, metadata)
    }

    pub(crate) fn record_confirmed_outcome(
        &mut self,
        command: &str,
        metadata: HistoryMetadata,
    ) -> Result<()> {
        self.drain_persistence_acks();
        let persistence_id = self.next_persistence_id();
        let context = get_current_context();
        if let Some(entry) = self
            .histories
            .iter_mut()
            .rev()
            .find(|entry| entry.entry == command)
        {
            if context.is_some() {
                entry.context.clone_from(&context);
            }
            entry.exit_code = metadata.exit_code;
            entry.duration_ms = metadata.duration_ms;
            entry.cwd = metadata.cwd.clone();
            entry.session_id = metadata.session_id.clone();
            entry.hostname = metadata.hostname.clone();
            Self::track_pending_entry(
                &mut self.pending_persistence,
                persistence_id,
                entry,
                1,
                None,
            );
        }

        let commands = vec![command.to_string()];
        if let Some(sender) = &self.sender {
            let _ = sender.send(HistoryMsg::RecordOutcome {
                command: command.to_string(),
                context,
                metadata,
                persistence_id,
            });
        } else if let Some(db) = &mut self.db {
            if Self::record_outcome_sync(db, command, context, &metadata).is_ok() {
                self.acknowledge_persistence(persistence_id, &commands);
            }
        } else if let Some(writes) = &mut self.startup_writes {
            writes.push(StartupHistoryWrite::Outcome {
                command: command.to_string(),
                context,
                metadata,
                persistence_id,
            });
        }
        self.bump_revision();
        Ok(())
    }
}

#[cfg(test)]
#[path = "persist_tests.rs"]
mod tests;
