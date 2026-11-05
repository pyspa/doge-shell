//! Publish startup database snapshots into the live history without replacing
//! foreground writes, navigation, or the loaded background persistence owner.
use super::*;

impl StartupHistoryWrite {
    fn into_message(self) -> HistoryMsg {
        match self {
            Self::Batch {
                entries,
                context,
                persistence_id,
                commands,
            } => HistoryMsg::WriteBatch {
                entries,
                context,
                persistence_id,
                commands,
            },
            Self::Outcome {
                command,
                context,
                metadata,
                persistence_id,
            } => HistoryMsg::RecordOutcome {
                command,
                context,
                metadata,
                persistence_id,
            },
        }
    }
}

impl History {
    /// The loader has already read SQLite and started its canonical writer.
    /// Only in-memory merge work and channel sends happen under the live lock.
    pub(crate) fn install_startup_history(&mut self, loaded: &History, mut older: Vec<Entry>) {
        debug_assert!(self.db.is_none());
        self.db = loaded.db.clone();
        self.sender = loaded.sender.clone();
        self.persist_ack_rx = loaded.persist_ack_rx.clone();
        older.extend(loaded.histories.iter().cloned());
        self.merge_startup_baseline(older);

        if let Some(sender) = &self.sender {
            let writes = self.startup_writes.take().unwrap_or_default();
            let mut writes = writes.into_iter();
            while let Some(write) = writes.next() {
                // Preserve failed handoff state; successful messages belong to
                // the writer and must never be replayed a second time.
                if sender.send(write.clone().into_message()).is_err() {
                    self.startup_writes = Some(std::iter::once(write).chain(writes).collect());
                    tracing::warn!("startup history writer disconnected during handoff");
                    break;
                }
            }
        }
    }

    /// A terminal failure has no persistence or retry owner. Keep the aggregate
    /// entries and pending snapshots, but release every original output payload
    /// and prevent later memory-only writes from growing an operation journal.
    pub(crate) fn end_startup_initialization(&mut self) {
        self.startup_writes = None;
    }

    /// Both snapshots predate foreground writes. Existing entries contain the
    /// local delta, so add the database baseline exactly once, keeping all live
    /// metadata. Merge the combined baseline once before any reload can apply.
    fn merge_startup_baseline(&mut self, entries: Vec<Entry>) {
        let selected = self
            .histories
            .get(self.current_index)
            .map(|entry| entry.entry.clone());
        let mut baseline: HashMap<_, _> = entries
            .into_iter()
            .map(|entry| (entry.entry.clone(), entry))
            .collect();
        let mut merged = Vec::with_capacity(baseline.len() + self.histories.len());
        for mut local in std::mem::take(&mut self.histories) {
            if let Some(stored) = baseline.remove(&local.entry) {
                local.count += stored.count;
            }
            merged.push(local);
        }
        merged.extend(baseline.into_values());
        merged.sort_by(|left, right| {
            left.when
                .cmp(&right.when)
                .then(left.entry.cmp(&right.entry))
        });
        self.histories = merged;
        self.rebuild_normalized_entries();
        self.recent_cache = self
            .histories
            .iter()
            .rev()
            .take(100)
            .map(|entry| entry.entry.clone())
            .collect();
        self.recent_cache.reverse();
        self.current_index = selected
            .and_then(|command| {
                self.histories
                    .iter()
                    .position(|entry| entry.entry == command)
            })
            .unwrap_or(self.histories.len());
        for entry in &self.histories {
            if let Some(pending) = self.pending_persistence.get_mut(&entry.entry) {
                pending.entry = entry.clone();
            }
        }
        self.bump_revision();
    }
}
