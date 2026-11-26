use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, functions::FunctionFlags};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn new(path: PathBuf) -> Result<Self> {
        let conn = Connection::open(&path).context("Failed to open SQLite database")?;
        Self::initialize(path, conn)
    }

    pub(crate) fn open_read_only(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Self::with_history_filter(conn)
    }

    fn initialize(path: PathBuf, conn: Connection) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::fs;
            use std::os::unix::fs::PermissionsExt;

            let metadata = fs::metadata(&path).context("Failed to read SQLite metadata")?;
            let mut perms = metadata.permissions();
            // Restrict database access to current user.
            perms.set_mode(0o600);
            fs::set_permissions(&path, perms).context("Failed to set SQLite permissions")?;
        }

        // Optimize SQLite settings for performance
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;",
        )?;

        let db = Self::with_history_filter(conn)?;
        db.init_schema()?;
        Ok(db)
    }

    fn with_history_filter(conn: Connection) -> Result<Self> {
        // Older databases can contain chat turns. Apply the same policy before
        // each query's LIMIT without deleting existing user data on startup.
        conn.create_scalar_function(
            "dogesh_history_command",
            1,
            FunctionFlags::SQLITE_UTF8
                | FunctionFlags::SQLITE_DETERMINISTIC
                | FunctionFlags::SQLITE_DIRECTONLY,
            |ctx| {
                Ok(ctx
                    .get_raw(0)
                    .as_str()
                    .is_ok_and(crate::history::is_legacy_command_history_input))
            },
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Command History Table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS command_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                command TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                context TEXT,
                exit_code INTEGER,
                duration_ms INTEGER,
                cwd TEXT,
                session_id TEXT,
                hostname TEXT,
                count INTEGER DEFAULT 1,
                is_command INTEGER
            )",
            [],
        )?;

        // Migration: Add count column if it assumes to be missing (naive check via error)
        let _ = conn.execute(
            "ALTER TABLE command_history ADD COLUMN count INTEGER DEFAULT 1",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE command_history ADD COLUMN exit_code INTEGER",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE command_history ADD COLUMN duration_ms INTEGER",
            [],
        );
        let _ = conn.execute("ALTER TABLE command_history ADD COLUMN cwd TEXT", []);
        let _ = conn.execute("ALTER TABLE command_history ADD COLUMN session_id TEXT", []);
        let _ = conn.execute("ALTER TABLE command_history ADD COLUMN hostname TEXT", []);

        // Append-only execution ledger. This deliberately coexists with the
        // aggregate command_history table so existing databases and frecency
        // behavior remain unchanged.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS command_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                command TEXT NOT NULL,
                cwd TEXT,
                started_at INTEGER NOT NULL,
                duration_ms INTEGER,
                exit_code INTEGER,
                session_id TEXT,
                hostname TEXT,
                author TEXT NOT NULL DEFAULT 'human',
                output TEXT,
                is_command INTEGER
            )",
            [],
        )?;
        // NULL means this row predates the input policy. New writers confirm
        // commands at their source, including foreign shells where `! cmd`
        // may be an ordinary command. Older dsh writers still insert NULL.
        for table in ["command_history", "command_events"] {
            if !conn.column_exists(None, table, "is_command")?
                && let Err(error) = conn.execute(
                    &format!("ALTER TABLE {table} ADD COLUMN is_command INTEGER"),
                    [],
                )
                && !conn.column_exists(None, table, "is_command")?
            {
                return Err(error.into());
            }
        }
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_command_events_started_at ON command_events(started_at DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_command_events_author ON command_events(author, started_at DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_command_events_session ON command_events(session_id, started_at DESC)",
            [],
        )?;

        // Directory Visits Log (Append Only)
        conn.execute(
            "CREATE TABLE IF NOT EXISTS directory_visits (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                context TEXT
            )",
            [],
        )?;

        // Directory Snapshot (Cache for Frecency)
        conn.execute(
            "CREATE TABLE IF NOT EXISTS directory_snapshot (
                path TEXT PRIMARY KEY,
                score REAL NOT NULL,
                last_accessed INTEGER NOT NULL,
                access_count INTEGER NOT NULL,
                half_life REAL NOT NULL,
                context TEXT,
                score_time REAL
            )",
            [],
        )?;

        // Older snapshots have no score timestamp. Decode their stored score
        // as the load-time score until the next visit upgrades that row.
        if !conn.column_exists(None, "directory_snapshot", "score_time")? {
            let result = conn.execute(
                "ALTER TABLE directory_snapshot ADD COLUMN score_time REAL",
                [],
            );
            // Another shell may have completed the same migration meanwhile.
            if result.is_err() && !conn.column_exists(None, "directory_snapshot", "score_time")? {
                result?;
            }
        }

        // Create indexes for performance
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_command_history_timestamp ON command_history(timestamp DESC)",
            [],
        )?;
        // Create unique index on command for UPSERT support
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_command_history_command ON command_history(command)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_directory_visits_timestamp ON directory_visits(timestamp DESC)",
            [],
        )?;

        // Snippets Table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS snippets (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                command TEXT NOT NULL,
                description TEXT,
                tags TEXT,
                created_at INTEGER NOT NULL,
                last_used INTEGER,
                use_count INTEGER DEFAULT 0
            )",
            [],
        )?;

        // Create index for snippet name lookups
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_snippets_name ON snippets(name)",
            [],
        )?;

        // Bookmarks Table (for command bookmarking)
        conn.execute(
            "CREATE TABLE IF NOT EXISTS bookmarks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                command TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                use_count INTEGER DEFAULT 0
            )",
            [],
        )?;

        // Directory Aliases Table (for z command enhancement)
        conn.execute(
            "CREATE TABLE IF NOT EXISTS dir_aliases (
                name TEXT PRIMARY KEY,
                path TEXT NOT NULL
            )",
            [],
        )?;

        Ok(())
    }

    pub fn get_connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn additive_migration_preserves_aggregate_history_and_adds_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE command_history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    command TEXT NOT NULL,
                    timestamp INTEGER NOT NULL,
                    context TEXT,
                    count INTEGER DEFAULT 1
                );
                INSERT INTO command_history(command, timestamp) VALUES ('git status', 1);",
            )
            .unwrap();
        }

        let db = Db::new(path).unwrap();
        let conn = db.get_connection();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM command_history", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let events_table: String = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='command_events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(events_table, "command_events");
    }
}
