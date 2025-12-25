use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn new(path: PathBuf) -> Result<Self> {
        let conn = Connection::open(path).context("Failed to open SQLite database")?;

        let db = Db {
            conn: Arc::new(Mutex::new(conn)),
        };

        db.init_schema()?;

        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();

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
                count INTEGER DEFAULT 1
            )",
            [],
        )?;

        // Migration: Add count column if it assumes to be missing (naive check via error)
        let _ = conn.execute(
            "ALTER TABLE command_history ADD COLUMN count INTEGER DEFAULT 1",
            [],
        );

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
                context TEXT
            )",
            [],
        )?;

        // Create indexes for performance
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_command_history_timestamp ON command_history(timestamp DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_directory_visits_timestamp ON directory_visits(timestamp DESC)",
            [],
        )?;

        Ok(())
    }

    pub fn get_connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }
}
