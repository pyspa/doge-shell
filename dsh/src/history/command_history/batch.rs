//! Apply timestamped command occurrences to the in-memory history and its caches.
use super::merge::{CountMerge, merge_entries};
use super::*;

struct Occurrences {
    newest: i64,
    position: usize,
    count: usize,
}

impl History {
    pub(super) fn recent_commands(entries: &[Entry]) -> Vec<String> {
        entries[entries.len().saturating_sub(100)..]
            .iter()
            .map(|entry| entry.entry.clone())
            .collect()
    }

    pub(super) fn apply_batch(
        &mut self,
        entries: &[(String, i64)],
        context: &Option<String>,
        persistence_id: u64,
    ) {
        if let [(command, when)] = entries {
            self.apply_single(command, *when, context, persistence_id);
            return;
        }
        if entries.is_empty() {
            return;
        }

        // Collapse repeated commands before touching the accumulated history.
        // Position records the last occurrence at the winning timestamp.
        let mut occurrences = HashMap::new();
        for (position, (command, when)) in entries.iter().enumerate() {
            let batch = occurrences.entry(command.as_str()).or_insert(Occurrences {
                newest: *when,
                position,
                count: 0,
            });
            batch.count += 1;
            if *when >= batch.newest {
                batch.newest = *when;
                batch.position = position;
            }
        }
        let updates = occurrences.iter().map(|(&command, batch)| {
            (
                batch.position,
                Self::new_occurrence(command, batch.newest, batch.count as i64, context),
                CountMerge::Add,
            )
        });
        self.histories = merge_entries(std::mem::take(&mut self.histories), updates);
        for entry in &self.histories {
            if let Some(batch) = occurrences.get(entry.entry.as_str()) {
                Self::track_pending_entry(
                    &mut self.pending_persistence,
                    persistence_id,
                    entry,
                    batch.count,
                    Some((batch.newest, persistence_id, batch.position)),
                );
            }
        }
        self.rebuild_normalized_entries();
        self.recent_cache = Self::recent_commands(&self.histories);
    }

    /// Interactive writes only move one row. Keep the aligned search cache
    /// and avoid rebuilding a batch index or sorting the entire history.
    fn apply_single(
        &mut self,
        command: &str,
        when: i64,
        context: &Option<String>,
        persistence_id: u64,
    ) {
        if self.normalized_entries.len() != self.histories.len() {
            self.rebuild_normalized_entries();
        }
        let existing = self
            .histories
            .iter()
            .rposition(|entry| entry.entry == command);
        let mut count = 1;
        let index = if let Some(index) = existing
            && self.histories[index].when > when
        {
            self.histories[index].count += 1;
            index
        } else {
            if let Some(index) = existing {
                count += self.histories.remove(index).count;
                self.normalized_entries.remove(index);
            }
            let index = self.histories.partition_point(|entry| entry.when <= when);
            self.histories
                .insert(index, Self::new_occurrence(command, when, count, context));
            self.normalized_entries
                .insert(index, Self::normalized_command(command));
            if index + 1 == self.histories.len() {
                self.recent_cache.retain(|entry| entry != command);
                self.recent_cache.push(command.to_string());
                if self.recent_cache.len() > 100 {
                    self.recent_cache.remove(0);
                }
            } else {
                self.recent_cache = Self::recent_commands(&self.histories);
            }
            index
        };
        Self::track_pending_entry(
            &mut self.pending_persistence,
            persistence_id,
            &self.histories[index],
            1,
            Some((when, persistence_id, 0)),
        );
    }

    fn new_occurrence(command: &str, when: i64, count: i64, context: &Option<String>) -> Entry {
        Entry {
            entry: command.to_string(),
            when,
            count,
            context: context.clone(),
            exit_code: None,
            duration_ms: None,
            cwd: None,
            session_id: None,
            hostname: None,
        }
    }
}
