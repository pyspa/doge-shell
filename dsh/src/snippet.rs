use crate::db::Db;
use anyhow::Result;
use chrono::Local;
use serde::{Deserialize, Serialize};

/// Represents a command snippet
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub id: i64,
    pub name: String,
    pub command: String,
    pub description: Option<String>,
    pub tags: Option<String>,
    pub created_at: i64,
    pub last_used: Option<i64>,
    pub use_count: i64,
}

impl Snippet {
    pub fn new(name: &str, command: &str, description: Option<&str>) -> Self {
        Snippet {
            id: 0,
            name: name.to_string(),
            command: command.to_string(),
            description: description.map(|s| s.to_string()),
            tags: None,
            created_at: Local::now().timestamp(),
            last_used: None,
            use_count: 0,
        }
    }
}

/// Manages command snippets with SQLite storage
#[derive(Debug, Clone)]
pub struct SnippetManager {
    db: Option<Db>,
}

impl Default for SnippetManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SnippetManager {
    pub fn new() -> Self {
        SnippetManager { db: None }
    }

    pub fn with_db(db: Db) -> Self {
        SnippetManager { db: Some(db) }
    }

    /// Add a new snippet
    pub fn add(&self, name: &str, command: &str, description: Option<&str>) -> Result<()> {
        if let Some(db) = &self.db {
            let conn = db.get_connection();
            let now = Local::now().timestamp();
            conn.execute(
                "INSERT INTO snippets (name, command, description, created_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![name, command, description, now],
            )?;
        }
        Ok(())
    }

    /// Remove a snippet by name
    pub fn remove(&self, name: &str) -> Result<bool> {
        if let Some(db) = &self.db {
            let conn = db.get_connection();
            let rows = conn.execute("DELETE FROM snippets WHERE name = ?1", [name])?;
            return Ok(rows > 0);
        }
        Ok(false)
    }

    /// Get a snippet by name
    pub fn get(&self, name: &str) -> Result<Option<Snippet>> {
        if let Some(db) = &self.db {
            let conn = db.get_connection();
            let mut stmt = conn.prepare(
                "SELECT id, name, command, description, tags, created_at, last_used, use_count 
                 FROM snippets WHERE name = ?1",
            )?;

            let snippet = stmt
                .query_row([name], |row| {
                    Ok(Snippet {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        command: row.get(2)?,
                        description: row.get(3)?,
                        tags: row.get(4)?,
                        created_at: row.get(5)?,
                        last_used: row.get(6)?,
                        use_count: row.get(7)?,
                    })
                })
                .ok();

            return Ok(snippet);
        }
        Ok(None)
    }

    /// List all snippets
    pub fn list(&self) -> Result<Vec<Snippet>> {
        let mut snippets = Vec::new();
        if let Some(db) = &self.db {
            let conn = db.get_connection();
            let mut stmt = conn.prepare(
                "SELECT id, name, command, description, tags, created_at, last_used, use_count 
                 FROM snippets ORDER BY use_count DESC, name ASC",
            )?;

            let rows = stmt.query_map([], |row| {
                Ok(Snippet {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    command: row.get(2)?,
                    description: row.get(3)?,
                    tags: row.get(4)?,
                    created_at: row.get(5)?,
                    last_used: row.get(6)?,
                    use_count: row.get(7)?,
                })
            })?;

            for row in rows.flatten() {
                snippets.push(row);
            }
        }
        Ok(snippets)
    }

    /// Update snippet usage statistics
    pub fn record_use(&self, name: &str) -> Result<()> {
        if let Some(db) = &self.db {
            let conn = db.get_connection();
            let now = Local::now().timestamp();
            conn.execute(
                "UPDATE snippets SET use_count = use_count + 1, last_used = ?1 WHERE name = ?2",
                rusqlite::params![now, name],
            )?;
        }
        Ok(())
    }

    /// Update a snippet's command and description
    pub fn update(&self, name: &str, command: &str, description: Option<&str>) -> Result<bool> {
        if let Some(db) = &self.db {
            let conn = db.get_connection();
            let rows = conn.execute(
                "UPDATE snippets SET command = ?1, description = ?2 WHERE name = ?3",
                rusqlite::params![command, description, name],
            )?;
            return Ok(rows > 0);
        }
        Ok(false)
    }

    /// Search snippets by name or command pattern
    pub fn search(&self, pattern: &str) -> Result<Vec<Snippet>> {
        let mut snippets = Vec::new();
        if let Some(db) = &self.db {
            let conn = db.get_connection();
            let like_pattern = format!("%{}%", pattern);
            let mut stmt = conn.prepare(
                "SELECT id, name, command, description, tags, created_at, last_used, use_count 
                 FROM snippets 
                 WHERE name LIKE ?1 OR command LIKE ?1 OR description LIKE ?1
                 ORDER BY use_count DESC, name ASC",
            )?;

            let rows = stmt.query_map([&like_pattern], |row| {
                Ok(Snippet {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    command: row.get(2)?,
                    description: row.get(3)?,
                    tags: row.get(4)?,
                    created_at: row.get(5)?,
                    last_used: row.get(6)?,
                    use_count: row.get(7)?,
                })
            })?;

            for row in rows.flatten() {
                snippets.push(row);
            }
        }
        Ok(snippets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snippet_new() {
        let snippet = Snippet::new("test", "echo hello", Some("A test snippet"));
        assert_eq!(snippet.name, "test");
        assert_eq!(snippet.command, "echo hello");
        assert_eq!(snippet.description, Some("A test snippet".to_string()));
        assert_eq!(snippet.use_count, 0);
    }
}
