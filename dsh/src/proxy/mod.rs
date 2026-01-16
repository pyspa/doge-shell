//! Shell proxy implementation for builtin command dispatch.
//!
//! This module provides the `ShellProxy` trait implementation for `Shell`,
//! routing builtin commands to their respective handlers.

mod builtin;
mod external;

use crate::shell::Shell;
use anyhow::{Context as _, Result};
use dsh_builtin::ShellProxy;
use dsh_types::{Context, mcp::McpServerConfig};
use globmatch;
use tracing::{debug, warn};

impl ShellProxy for Shell {
    fn exit_shell(&mut self) {
        self.exit();
    }

    fn get_github_status(&self) -> (usize, usize, usize) {
        if let Some(ref status) = self.github_status {
            let status = status.read();
            (
                status.review_count,
                status.mention_count,
                status.other_count,
            )
        } else {
            (0, 0, 0)
        }
    }

    fn get_git_branch(&self) -> Option<String> {
        let output = std::process::Command::new("git")
            .arg("branch")
            .arg("--show-current")
            .output()
            .ok()?;
        if output.status.success() {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if branch.is_empty() {
                None
            } else {
                Some(branch)
            }
        } else {
            None
        }
    }

    fn get_job_count(&self) -> usize {
        self.wait_jobs.len()
    }

    fn save_path_history(&mut self, path: &str) {
        // Check exclusions
        {
            let env = self.environment.read();
            for pattern in &env.z_exclude {
                if let Ok(matcher) = globmatch::Builder::new(pattern).build("/")
                    && matcher.is_match(path.into())
                {
                    debug!("dsh: path rejected by z_exclude: {}", path);
                    return;
                }
            }
        }

        if let Some(ref mut history) = self.path_history {
            let mut history = history.lock();
            history.add(path);
            history.save_background();
        }
    }

    fn save_output_history(&mut self, entry: dsh_types::output_history::OutputEntry) {
        self.environment.write().output_history.push(entry);
    }

    fn changepwd(&mut self, path: &str) -> Result<()> {
        // Save current directory as OLDPWD before changing
        if let Ok(current) = std::env::current_dir() {
            let old_pwd = current.to_string_lossy().into_owned();
            self.environment
                .write()
                .variables
                .insert("OLDPWD".to_string(), old_pwd);
        }

        std::env::set_current_dir(path)?;

        // Use the canonical path we actually landed in for history and hooks
        let final_path = if let Ok(canon) = std::env::current_dir() {
            canon.to_string_lossy().into_owned()
        } else {
            path.to_string()
        };

        self.save_path_history(&final_path);
        self.exec_chpwd_hooks(&final_path)?;
        Ok(())
    }

    fn insert_path(&mut self, idx: usize, path: &str) {
        self.environment.write().paths.insert(idx, path.to_string());
    }

    fn dispatch(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()> {
        use builtin::registry::BUILTIN_REGISTRY;

        if let Some(handler) = BUILTIN_REGISTRY.get(cmd) {
            handler(self, ctx, argv)
        } else {
            external::execute(ctx, cmd, argv)
        }
    }

    fn get_var(&mut self, key: &str) -> Option<String> {
        self.environment.read().get_var(key)
    }

    fn get_lisp_var(&self, key: &str) -> Option<String> {
        let lisp_engine = self.lisp_engine.borrow();
        let env = lisp_engine.env.borrow();
        match env.get(&crate::lisp::Symbol::from(key)) {
            Some(crate::lisp::Value::String(s)) => Some(s.clone()),
            Some(crate::lisp::Value::Int(i)) => Some(i.to_string()),
            _ => None,
        }
    }

    fn set_var(&mut self, key: String, value: String) {
        self.environment.write().variables.insert(key, value);
    }

    fn set_env_var(&mut self, key: String, value: String) {
        if key == "PATH" {
            let mut path_vec = vec![];
            for value in value.split(':') {
                path_vec.push(value.to_string());
            }
            let env_path = path_vec.join(":");
            unsafe { std::env::set_var("PATH", &env_path) };
            debug!("set env {} {}", &key, &env_path);
            self.environment.write().reload_path();
        } else {
            unsafe { std::env::set_var(&key, &value) };
            debug!("set env {} {}", &key, &value);
        }
    }

    fn unset_env_var(&mut self, key: &str) {
        unsafe { std::env::remove_var(key) };
        debug!("unset env {}", key);
        if key == "PATH" {
            self.environment.write().reload_path();
        }
    }

    fn get_alias(&mut self, name: &str) -> Option<String> {
        debug!("Getting alias for: {}", name);
        self.environment.read().alias.get(name).cloned()
    }

    fn set_alias(&mut self, name: String, command: String) {
        debug!("Setting alias: {} = {}", name, command);
        self.environment.write().alias.insert(name, command);
    }

    fn list_aliases(&mut self) -> std::collections::HashMap<String, String> {
        debug!("Listing all aliases");
        self.environment.read().alias.clone()
    }

    fn add_abbr(&mut self, name: String, expansion: String) {
        debug!("Adding abbreviation: {} = {}", name, expansion);
        self.environment
            .write()
            .abbreviations
            .insert(name, expansion);
    }

    fn remove_abbr(&mut self, name: &str) -> bool {
        debug!("Removing abbreviation: {}", name);
        self.environment
            .write()
            .abbreviations
            .remove(name)
            .is_some()
    }

    fn list_abbrs(&self) -> Vec<(String, String)> {
        debug!("Listing all abbreviations");
        self.environment
            .read()
            .abbreviations
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn get_abbr(&self, name: &str) -> Option<String> {
        debug!("Getting abbreviation for: {}", name);
        self.environment.read().abbreviations.get(name).cloned()
    }

    fn list_mcp_servers(&mut self) -> Vec<McpServerConfig> {
        self.environment.read().mcp_servers().to_vec()
    }

    fn list_execute_allowlist(&mut self) -> Vec<String> {
        self.environment.read().execute_allowlist().to_vec()
    }

    fn run_hook(&mut self, hook_name: &str, args: Vec<String>) -> Result<()> {
        let args_str = args
            .iter()
            .map(|a| format!("\"{}\"", a.replace("\"", "\\\"")))
            .collect::<Vec<_>>()
            .join(" ");

        // Ensure hook name is wrapped in asterisks for Lisp convention
        let hook_var = if hook_name.starts_with('*') {
            hook_name.to_string()
        } else {
            format!("*{}*", hook_name)
        };

        let lisp_code = format!(
            "(when (bound? '{hook_var})
                (map (lambda (hook) (hook {args_str})) {hook_var}))"
        );

        if let Err(e) = self.lisp_engine.borrow().run(&lisp_code) {
            // We use warn! but return Ok because hook failure shouldn't crash the command
            warn!("Failed to execute hook {}: {}", hook_name, e);
        }
        Ok(())
    }

    fn select_item(&mut self, items: Vec<String>) -> Result<Option<String>> {
        let candidates: Vec<crate::completion::Candidate> = items
            .into_iter()
            .map(|item| crate::completion::Candidate::Item(item, "".to_string()))
            .collect();

        Ok(crate::completion::select_item_with_skim(candidates, None))
    }

    // New method implementations for export
    fn list_exported_vars(&self) -> Vec<(String, String)> {
        let env = self.environment.read();
        env.exported_vars
            .iter()
            .filter_map(|key| {
                env.variables
                    .get(key)
                    .map(|value| (key.clone(), value.clone()))
            })
            .collect()
    }

    fn export_var(&mut self, key: &str) -> bool {
        let mut env = self.environment.write();
        if env.variables.contains_key(key) {
            env.exported_vars.insert(key.to_string());
            true
        } else {
            // Also allow exporting non-existent variables, they will be exported if set later.
            env.exported_vars.insert(key.to_string());
            false
        }
    }

    fn set_and_export_var(&mut self, key: String, value: String) {
        let mut env = self.environment.write();
        env.variables.insert(key.clone(), value);
        env.exported_vars.insert(key);
    }

    fn get_current_dir(&self) -> Result<std::path::PathBuf> {
        std::env::current_dir().context("failed to get current directory")
    }

    fn confirm_action(&mut self, message: &str) -> Result<bool> {
        use std::io::stdin;

        debug!("Safety confirmation requested: {}", message);

        // Use eprint! instead of println! or print! to ensure the prompt goes to stderr.
        // This is critical if the shell output is being piped.
        eprint!("{} [y/N]: ", message);
        use std::io::Write;
        std::io::stderr().flush()?;

        let mut input = String::new();
        stdin().read_line(&mut input)?;

        let confirmed = input.trim().to_lowercase() == "y";
        debug!("Confirmation result: {}", confirmed);
        Ok(confirmed)
    }

    fn is_canceled(&self) -> bool {
        crate::process::signal::check_and_clear_sigint()
    }

    fn get_full_output_history(&self) -> Vec<dsh_types::output_history::OutputEntry> {
        self.environment.read().output_history.get_all_entries()
    }

    fn capture_command(&mut self, _ctx: &Context, cmd: &str) -> Result<(i32, String, String)> {
        use std::process::{Command, Stdio};

        // We implement this synchronously to avoid 'Cannot start a runtime from within a runtime' panic.
        debug!("Capturing command: '{}'", cmd);

        // Use sh -c to execute the command
        let output = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("Failed to capture command: {}", cmd))?;

        let exit_code = output.status.code().unwrap_or(1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        Ok((exit_code, stdout, stderr))
    }

    fn generate_command_completion(
        &mut self,
        command_name: &str,
        help_text: &str,
    ) -> Result<String> {
        let command_name = command_name.to_string();
        let help_text = help_text.to_string();

        let ai_service = self.environment.read().ai_service.clone();
        if let Some(service) = ai_service {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                tokio::task::block_in_place(move || {
                    handle.block_on(async move {
                        crate::ai_features::generate_completion_json(
                            service.as_ref(),
                            &command_name,
                            &help_text,
                        )
                        .await
                    })
                })
            } else {
                let runtime = tokio::runtime::Runtime::new()?;
                runtime.block_on(async move {
                    crate::ai_features::generate_completion_json(
                        service.as_ref(),
                        &command_name,
                        &help_text,
                    )
                    .await
                })
            }
        } else {
            Err(anyhow::anyhow!("AI service not available"))
        }
    }

    fn ask_ai(&mut self, messages: Vec<serde_json::Value>) -> Result<String> {
        let ai_service = self.environment.read().ai_service.clone();
        if let Some(service) = ai_service {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                tokio::task::block_in_place(move || {
                    handle.block_on(async move { service.send_request(messages, Some(0.7)).await })
                })
            } else {
                let runtime = tokio::runtime::Runtime::new()?;
                runtime.block_on(async move { service.send_request(messages, Some(0.7)).await })
            }
        } else {
            Err(anyhow::anyhow!("AI service not available"))
        }
    }

    fn open_editor(&mut self, content: &str, extension: &str) -> Result<String> {
        crate::utils::editor::open_editor(content, extension)
    }

    fn add_snippet(&mut self, name: String, command: String, description: Option<String>) -> bool {
        match crate::environment::get_data_file("dsh_snippets.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let manager = crate::snippet::SnippetManager::with_db(db);
                    manager.add(&name, &command, description.as_deref()).is_ok()
                }
                Err(e) => {
                    warn!("Failed to open snippet database: {}", e);
                    false
                }
            },
            Err(e) => {
                warn!("Failed to get snippet database path: {}", e);
                false
            }
        }
    }

    fn remove_snippet(&mut self, name: &str) -> bool {
        match crate::environment::get_data_file("dsh_snippets.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let manager = crate::snippet::SnippetManager::with_db(db);
                    manager.remove(name).unwrap_or(false)
                }
                Err(e) => {
                    warn!("Failed to open snippet database: {}", e);
                    false
                }
            },
            Err(e) => {
                warn!("Failed to get snippet database path: {}", e);
                false
            }
        }
    }

    fn list_snippets(&self) -> Vec<dsh_types::snippet::Snippet> {
        match crate::environment::get_data_file("dsh_snippets.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let manager = crate::snippet::SnippetManager::with_db(db);
                    manager
                        .list()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|s| dsh_types::snippet::Snippet {
                            id: s.id,
                            name: s.name,
                            command: s.command,
                            description: s.description,
                            tags: s.tags,
                            created_at: s.created_at,
                            last_used: s.last_used,
                            use_count: s.use_count,
                        })
                        .collect()
                }
                Err(e) => {
                    warn!("Failed to open snippet database: {}", e);
                    Vec::new()
                }
            },
            Err(e) => {
                warn!("Failed to get snippet database path: {}", e);
                Vec::new()
            }
        }
    }

    fn get_snippet(&self, name: &str) -> Option<dsh_types::snippet::Snippet> {
        match crate::environment::get_data_file("dsh_snippets.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let manager = crate::snippet::SnippetManager::with_db(db);
                    manager
                        .get(name)
                        .ok()
                        .flatten()
                        .map(|s| dsh_types::snippet::Snippet {
                            id: s.id,
                            name: s.name,
                            command: s.command,
                            description: s.description,
                            tags: s.tags,
                            created_at: s.created_at,
                            last_used: s.last_used,
                            use_count: s.use_count,
                        })
                }
                Err(e) => {
                    warn!("Failed to open snippet database: {}", e);
                    None
                }
            },
            Err(e) => {
                warn!("Failed to get snippet database path: {}", e);
                None
            }
        }
    }

    fn update_snippet(&mut self, name: &str, command: &str, description: Option<&str>) -> bool {
        match crate::environment::get_data_file("dsh_snippets.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let manager = crate::snippet::SnippetManager::with_db(db);
                    manager.update(name, command, description).unwrap_or(false)
                }
                Err(e) => {
                    warn!("Failed to open snippet database: {}", e);
                    false
                }
            },
            Err(e) => {
                warn!("Failed to get snippet database path: {}", e);
                false
            }
        }
    }

    fn record_snippet_use(&mut self, name: &str) {
        if let Ok(db_path) = crate::environment::get_data_file("dsh_snippets.db")
            && let Ok(db) = crate::db::Db::new(db_path)
        {
            let manager = crate::snippet::SnippetManager::with_db(db);
            let _ = manager.record_use(name);
        }
    }

    fn add_bookmark(&mut self, name: String, command: String) -> bool {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    let now = chrono::Utc::now().timestamp();
                    conn.execute(
                        "INSERT INTO bookmarks (name, command, created_at, use_count) VALUES (?1, ?2, ?3, 0)",
                        rusqlite::params![name, command, now],
                    ).is_ok()
                }
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    fn remove_bookmark(&mut self, name: &str) -> bool {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    conn.execute(
                        "DELETE FROM bookmarks WHERE name = ?1",
                        rusqlite::params![name],
                    )
                    .map(|c| c > 0)
                    .unwrap_or(false)
                }
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    fn list_bookmarks(&self) -> Vec<(String, String, i64)> {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    let mut stmt = match conn.prepare(
                        "SELECT name, command, use_count FROM bookmarks ORDER BY use_count DESC, name ASC",
                    ) {
                        Ok(s) => s,
                        Err(_) => return Vec::new(),
                    };
                    let rows = stmt.query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    });
                    rows.map(|r| r.filter_map(|r| r.ok()).collect())
                        .unwrap_or_default()
                }
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        }
    }

    fn get_bookmark(&self, name: &str) -> Option<(String, i64)> {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    conn.query_row(
                        "SELECT command, use_count FROM bookmarks WHERE name = ?1",
                        rusqlite::params![name],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .ok()
                }
                Err(_) => None,
            },
            Err(_) => None,
        }
    }

    fn record_bookmark_use(&mut self, name: &str) {
        if let Ok(db_path) = crate::environment::get_data_file("dsh.db")
            && let Ok(db) = crate::db::Db::new(db_path)
        {
            let conn = db.get_connection();
            let _ = conn.execute(
                "UPDATE bookmarks SET use_count = use_count + 1 WHERE name = ?1",
                rusqlite::params![name],
            );
        }
    }

    fn get_last_command(&self) -> Option<String> {
        if let Some(ref history) = self.cmd_history {
            let history = history.lock();
            history.iter().next().map(|e| e.entry.clone())
        } else {
            None
        }
    }

    fn add_dir_alias(&mut self, name: String, path: String) -> bool {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    conn.execute(
                        "INSERT OR REPLACE INTO dir_aliases (name, path) VALUES (?1, ?2)",
                        rusqlite::params![name, path],
                    )
                    .is_ok()
                }
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    fn remove_dir_alias(&mut self, name: &str) -> bool {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    conn.execute(
                        "DELETE FROM dir_aliases WHERE name = ?1",
                        rusqlite::params![name],
                    )
                    .map(|c| c > 0)
                    .unwrap_or(false)
                }
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    fn list_dir_aliases(&self) -> Vec<(String, String)> {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    let mut stmt =
                        match conn.prepare("SELECT name, path FROM dir_aliases ORDER BY name") {
                            Ok(s) => s,
                            Err(_) => return Vec::new(),
                        };
                    let rows = stmt.query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    });
                    rows.map(|r| r.filter_map(|r| r.ok()).collect())
                        .unwrap_or_default()
                }
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        }
    }

    fn get_dir_alias(&self, name: &str) -> Option<String> {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    conn.query_row(
                        "SELECT path FROM dir_aliases WHERE name = ?1",
                        rusqlite::params![name],
                        |row| row.get(0),
                    )
                    .ok()
                }
                Err(_) => None,
            },
            Err(_) => None,
        }
    }
}

// Re-export for backward compatibility
pub use builtin::jobs::parse_job_spec;
pub use builtin::reload::format_reload_error;
pub use builtin::z::parse_z_args;
