//! Bounded, metadata-only execution snapshot for the interactive history picker.
use super::*;

impl History {
    /// Never reads ledger output, including when output recording is enabled.
    /// Callers must gate this read on the current runtime ledger preference.
    pub fn picker_execution_metadata(&self, limit: usize) -> Result<Vec<CommandEvent>> {
        let Some(db) = &self.db else {
            return Ok(Vec::new());
        };
        let limit = limit.min(5000);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = db.get_connection();
        let mut stmt = conn.prepare(
            "SELECT id, command, cwd, started_at, duration_ms, exit_code, session_id, hostname, author
             FROM command_events ORDER BY started_at DESC, id DESC LIMIT ?1"
        )?;
        let events = stmt
            .query_map([limit as i64], |row| {
                Ok(CommandEvent {
                    id: row.get(0)?,
                    command: row.get(1)?,
                    cwd: row.get(2)?,
                    started_at: row.get(3)?,
                    duration_ms: row.get::<_, Option<i64>>(4)?.map(|n| n.max(0) as u64),
                    exit_code: row.get(5)?,
                    session_id: row.get(6)?,
                    hostname: row.get(7)?,
                    author: row.get(8)?,
                    output: None,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(events)
    }
}
