//! Track the greatest observed multiplicity of each fish command and source
//! timestamp, so importing overlapping snapshots only adds new occurrences.
use super::*;
use rusqlite::TransactionBehavior;
use sha2::{Digest, Sha256};

fn new_occurrences(
    entries: Vec<(Vec<u8>, Option<i64>)>,
    mut claim: impl FnMut(&[u8; 32], i64) -> Result<bool>,
) -> Result<Vec<(String, i64)>> {
    let now = Local::now().timestamp();
    let mut counts = HashMap::new();
    let mut new = Vec::new();
    for (command, when) in entries {
        // Fixed-width timestamp and presence flag distinguish missing time
        // from every real second. The receipt never stores the command text.
        let key: [u8; 32] = Sha256::new()
            .chain_update([u8::from(when.is_some())])
            .chain_update(when.unwrap_or_default().to_be_bytes())
            .chain_update(&command)
            .finalize()
            .into();
        let ordinal = counts.entry(key).or_insert(0);
        *ordinal += 1;
        if claim(&key, *ordinal)? {
            let command = String::from_utf8(command)
                .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
            new.push((command, when.unwrap_or(now)));
        }
    }
    Ok(new)
}

impl History {
    pub(crate) fn import_fish_entries(
        &mut self,
        entries: Vec<(Vec<u8>, Option<i64>)>,
    ) -> Result<usize> {
        if entries.is_empty() {
            return Ok(0);
        }
        let Some(db) = self.db.clone() else {
            let new = new_occurrences(entries, |key, ordinal| {
                let previous = self.memory_fish_imports.entry(*key).or_default();
                let changed = ordinal > *previous;
                *previous = (*previous).max(ordinal);
                Ok(changed)
            })?;
            let count = new.len();
            if count > 0 {
                self.write_batch(new)?;
            }
            return Ok(count);
        };

        if let Some(sender) = &self.sender {
            let (done, waited) = mpsc::channel();
            sender
                .send(HistoryMsg::Flush(done))
                .map_err(|_| anyhow::anyhow!("command history writer disconnected"))?;
            waited.recv()?;
        }
        self.drain_persistence_acks();
        let context = get_current_context();
        let mut conn = db.get_connection();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS fish_import_receipts (
                fingerprint BLOB PRIMARY KEY, occurrences INTEGER NOT NULL
             ) WITHOUT ROWID",
        )?;
        let new = {
            let mut receipt = tx.prepare(
                "INSERT INTO fish_import_receipts VALUES (?1, ?2)
                 ON CONFLICT(fingerprint) DO UPDATE SET occurrences = excluded.occurrences
                 WHERE excluded.occurrences > occurrences",
            )?;
            new_occurrences(entries, |key, ordinal| {
                Ok(receipt.execute(rusqlite::params![key, ordinal])? == 1)
            })?
        };
        if new.is_empty() {
            tx.commit()?;
            return Ok(0);
        }
        let mut baseline = self.read_import_baseline(&tx, &new)?;
        Self::upsert_batch(&tx, &new, context.as_deref())?;
        tx.commit()?;
        drop(conn);

        if !baseline.is_empty() {
            baseline.append(&mut self.histories);
            baseline.sort_by_key(|entry| entry.when);
            self.histories = baseline;
            self.rebuild_normalized_entries();
            self.recent_cache = Self::recent_commands(&self.histories);
        }
        let id = self.next_persistence_id();
        self.apply_batch(&new, &context, id);
        let commands: Vec<_> = new.iter().map(|(command, _)| command.clone()).collect();
        self.acknowledge_persistence(id, &commands);
        self.reset_index();
        self.bump_revision();
        Ok(new.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn a_failed_import_can_retry_the_whole_snapshot() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Db::new(dir.path().join("history.db"))?;
        let mut history = History {
            db: Some(db.clone()),
            ..Default::default()
        };
        db.get_connection().execute_batch(
            "CREATE TRIGGER fail_import BEFORE INSERT ON command_history WHEN NEW.command = 'second'
             BEGIN SELECT RAISE(ABORT, 'fixture'); END;"
        )?;
        let snapshot = vec![("first".into(), Some(1)), ("second".into(), Some(2))];
        assert!(history.import_fish_entries(snapshot.clone()).is_err());
        assert_eq!(history.iter().count(), 0);
        assert_eq!(
            db.get_connection()
                .query_row("SELECT count(*) FROM command_history", [], |row| row
                    .get::<_, i64>(0))?,
            0
        );
        db.get_connection()
            .execute_batch("DROP TRIGGER fail_import")?;
        assert_eq!(history.import_fish_entries(snapshot.clone())?, 2);
        assert_eq!(history.import_fish_entries(snapshot)?, 0);
        Ok(())
    }

    #[test]
    fn concurrent_imports_count_an_occurrence_once() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("history.db");
        let barrier = Barrier::new(2);
        let run = || {
            barrier.wait();
            let db = Db::new(path.clone())?;
            let mut history = History {
                db: Some(db),
                ..Default::default()
            };
            history.import_fish_entries(vec![("shared".into(), Some(42))])
        };
        let mut counts = std::thread::scope(|scope| -> Result<_> {
            let thread = scope.spawn(run);
            Ok([run()?, thread.join().unwrap()?])
        })?;
        let db = Db::new(path)?;
        counts.sort();
        assert_eq!(counts, [0, 1]);
        assert_eq!(
            db.get_connection().query_row(
                "SELECT count FROM command_history WHERE command = 'shared'",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            1
        );
        Ok(())
    }

    #[test]
    fn memory_only_import_distinguishes_missing_time_from_epoch_zero() -> Result<()> {
        let mut history = History::new();
        let entries = vec![("same".into(), None), ("same".into(), Some(0))];
        assert_eq!(history.import_fish_entries(entries.clone())?, 2);
        assert_eq!(history.import_fish_entries(entries)?, 0);
        assert_eq!(history.iter().next().unwrap().count, 2);
        Ok(())
    }
}
