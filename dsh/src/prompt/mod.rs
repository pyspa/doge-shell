use crate::environment::{ChangePwdHook, Environment};
use crate::github::GitHubStatus;
use anyhow::Result;
use crossterm::cursor;
use crossterm::queue;
use crossterm::style::Stylize;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub mod context;
pub mod modules;

use context::PromptContext;
use modules::PromptModule;
use modules::directory::DirectoryModule;
use modules::git::GitModule;
use modules::nodejs::NodeModule;
use modules::rust::RustModule;

// Re-export for compatibility
pub use crate::prompt::context::PromptContext as Context; // just in case

// Constants
const BRANCH_MARK: &str = "🐾";
#[allow(dead_code)]
const BRANCH_CONFLICT: &str = "🏴‍☠️";
const UNTRACKED: &str = "?";
const MODIFIED: &str = "!";
#[allow(dead_code)]
const NEW_FILE: &str = "+";

impl ChangePwdHook for Arc<RwLock<Prompt>> {
    fn call(&self, pwd: &Path, _env: Arc<RwLock<Environment>>) -> Result<()> {
        self.write().set_current(pwd);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    pub branch: String,
    pub branch_status: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub oid: Option<String>,
}

impl Default for GitStatus {
    fn default() -> Self {
        Self::new()
    }
}

impl GitStatus {
    pub fn new() -> Self {
        GitStatus {
            branch: "".to_string(),
            branch_status: None,
            ahead: 0,
            behind: 0,
            oid: None,
        }
    }
}

/// Git status cache structure
#[derive(Debug, Clone)]
struct GitStatusCache {
    status: GitStatus,
    last_updated: Instant,
    git_root: PathBuf,
    ttl: Duration,
}

impl GitStatusCache {
    fn new(status: GitStatus, git_root: PathBuf) -> Self {
        Self {
            status,
            last_updated: Instant::now(),
            git_root,
            ttl: Duration::from_secs(2), // Cache valid for 2 seconds
        }
    }

    fn is_valid(&self, current_git_root: &Path) -> bool {
        // Invalid if Git root changed
        if self.git_root != current_git_root {
            return false;
        }

        // Invalid if TTL exceeded
        self.last_updated.elapsed() < self.ttl
    }

    fn update(&mut self, status: GitStatus, git_root: PathBuf) {
        self.status = status;
        self.last_updated = Instant::now();
        self.git_root = git_root;
    }
}

#[derive(Debug)]
pub struct Prompt {
    pub current_dir: PathBuf,
    pub mark: String,
    pub github_status: Option<Arc<RwLock<GitHubStatus>>>,
    pub github_icon: String,
    current_git_root: Option<PathBuf>,
    pub needs_git_check: bool,
    git_root_cache: HashSet<String>,
    git_status_cache: Option<GitStatusCache>,

    // Language Support
    rust_version_cache: Option<String>,
    node_version_cache: Option<String>,

    // Module system
    modules: Vec<Box<dyn PromptModule>>,
}

impl Prompt {
    pub fn new(current_dir: PathBuf, mark: String) -> Prompt {
        let mut prompt = Prompt {
            current_dir: current_dir.clone(),
            mark: mark.clone(),
            github_status: None,
            github_icon: "🐙".to_string(),
            current_git_root: None,
            needs_git_check: true,
            git_root_cache: HashSet::new(),
            git_status_cache: None,
            rust_version_cache: None,
            node_version_cache: None,
            modules: vec![
                Box::new(DirectoryModule::new()),
                Box::new(GitModule::new(BRANCH_MARK.to_string())),
                Box::new(NodeModule::new()),
                Box::new(RustModule::new()),
            ],
        };

        // Set Git root during initialization
        prompt.set_current(&current_dir);
        prompt
    }

    pub fn print_preprompt<W: Write>(&mut self, out: &mut W) {
        write!(out, "{}", "\r".reset()).ok();

        // 1. Prepare Context
        // Check git status validity first (legacy logic adapted)

        let has_git = self.under_git();
        let mut status_to_display = self.get_git_status_cached();
        let real_branch = self.get_head_branch();

        // Branch verification logic (ported from original)
        if has_git && let Some(ref real) = real_branch {
            let mut cache_invalid = false;
            if let Some(ref status) = status_to_display {
                if status.branch != *real {
                    cache_invalid = true;
                }
            } else {
                cache_invalid = true;
            }

            if cache_invalid {
                if status_to_display.is_none() {
                    // Create a temporary status with the real branch name if we have nothing
                    let mut new_status = GitStatus::new();
                    new_status.branch = real.clone();
                    status_to_display = Some(new_status);
                }

                // Force invalidation of cache so async task picks it up
                self.git_status_cache = None;
                self.needs_git_check = true;
            }
        }

        let context = PromptContext {
            current_dir: self.current_dir.clone(),
            git_root: self.current_git_root.clone(),
            git_status: status_to_display,
            rust_version: self.rust_version_cache.clone(),
            node_version: self.node_version_cache.clone(),
        };

        // 2. Render Modules
        let mut prompt_content = String::new();

        for module in &self.modules {
            if let Some(content) = module.render(&context) {
                prompt_content.push_str(&content);
            }
        }

        // 3. GitHub Status (Internal Legacy - could be modularized later)
        // Display GitHub notifications if available and under git
        if has_git && let Some(status_lock) = &self.github_status {
            let status = status_lock.read();
            if status.notification_count > 0 {
                let notify_display = format!(
                    " {} {}",
                    self.github_icon.as_str().yellow(),
                    status.notification_count.to_string().yellow().bold()
                );
                prompt_content.push_str(&notify_display);
            } else if status.has_error {
                let notify_display = format!(" {}", "🔔!".red().bold());
                prompt_content.push_str(&notify_display);
            }
        }

        write!(out, "{}", prompt_content).ok();
    }

    pub fn print_right_prompt<W: Write>(
        &self,
        out: &mut W,
        cols: usize,
        last_status: i32,
        last_duration: Option<Duration>,
    ) {
        // Keep existing logic
        let time_str = chrono::Local::now().format("%H:%M:%S").to_string();

        let status_str = if last_status != 0 {
            format!("{} {} ", "✘".red().bold(), last_status.to_string().red())
        } else {
            String::new()
        };

        let duration_str = if let Some(d) = last_duration {
            if d.as_secs() >= 2 {
                let secs = d.as_secs();
                if secs < 60 {
                    format!("{}s ", secs)
                } else {
                    format!("{}m{}s ", secs / 60, secs % 60)
                }
                .yellow()
                .to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let right_prompt = format!("{}{}{}", status_str, duration_str, time_str.as_str().dim());
        let right_width = crate::input::display_width(&right_prompt);

        if cols > right_width + 1 {
            let start_col = cols - right_width - 1;
            queue!(
                out,
                cursor::MoveToColumn(start_col as u16),
                crossterm::style::Print(right_prompt),
                cursor::MoveToColumn(0)
            )
            .ok();
        }
    }

    // Helper methods (Keep mostly as is)

    pub fn under_git(&self) -> bool {
        if let Some(git_root) = &self.current_git_root {
            self.current_dir.starts_with(git_root)
        } else {
            false
        }
    }

    fn get_git_root_cached_only(&self) -> Option<String> {
        for git_root in &self.git_root_cache {
            if self.current_dir.starts_with(git_root) {
                return Some(git_root.to_string());
            }
        }
        None
    }

    pub fn current_path(&self) -> &Path {
        &self.current_dir
    }

    pub fn set_current(&mut self, path: &Path) {
        self.current_dir = path.to_path_buf();

        let mut root_changed = false;
        if let Some(git_root) = &self.current_git_root {
            if !self.current_dir.starts_with(git_root) {
                root_changed = true;
            }
        } else {
            root_changed = true;
        }

        if root_changed {
            if let Some(root) = self.get_git_root_cached_only() {
                self.current_git_root = Some(PathBuf::from(&root));
                self.git_status_cache = None;
                self.needs_git_check = false;
            } else {
                self.current_git_root = None;
                self.needs_git_check = true;
            }
        }
    }

    pub fn update_git_root(&mut self, root: Option<PathBuf>) {
        self.current_git_root = root.clone();
        if let Some(r) = root {
            self.git_root_cache.insert(r.to_string_lossy().to_string());
        }
        self.needs_git_check = false;
        self.git_status_cache = None;
    }

    pub fn get_git_status_cached(&self) -> Option<GitStatus> {
        let git_root = self.current_git_root.as_ref()?;
        let cache = self.git_status_cache.as_ref()?;

        if cache.is_valid(git_root) {
            Some(cache.status.clone())
        } else {
            if cache.git_root != *git_root {
                return None;
            }
            Some(cache.status.clone())
        }
    }

    pub fn should_refresh(&self) -> bool {
        let Some(git_root) = &self.current_git_root else {
            return false;
        };

        match &self.git_status_cache {
            Some(cache) => !cache.is_valid(git_root),
            None => true,
        }
    }

    pub fn update_git_status(&mut self, status: Option<GitStatus>) {
        let Some(git_root) = &self.current_git_root else {
            return;
        };

        if let Some(status) = status {
            if let Some(ref mut cache) = self.git_status_cache {
                cache.update(status, git_root.clone());
            } else {
                self.git_status_cache = Some(GitStatusCache::new(status, git_root.clone()));
            }
        }
    }

    fn get_head_branch(&self) -> Option<String> {
        let git_root = self.current_git_root.as_ref()?;
        let git_dir = git_root.join(".git");

        // Resolve .git file (worktree/submodule)
        let git_dir = if git_dir.is_file() {
            if let Ok(content) = std::fs::read_to_string(&git_dir) {
                if let Some(path) = content.trim().strip_prefix("gitdir: ") {
                    git_root.join(path.trim())
                } else {
                    return None;
                }
            } else {
                return None;
            }
        } else {
            git_dir
        };

        if !git_dir.exists() {
            return None;
        }

        let head_path = git_dir.join("HEAD");
        if let Ok(head_content) = std::fs::read_to_string(head_path) {
            let content = head_content.trim();
            if let Some(branch_ref) = content.strip_prefix("ref: refs/heads/") {
                return Some(branch_ref.to_string());
            } else {
                if content.len() >= 7 {
                    return Some(content[..7].to_string());
                }
                return Some("DETACHED".to_string());
            }
        }
        None
    }
    pub fn update_rust_version(&mut self, version: Option<String>) {
        self.rust_version_cache = version;
    }

    pub fn update_node_version(&mut self, version: Option<String>) {
        self.node_version_cache = version;
    }

    pub fn needs_rust_check(&self) -> bool {
        self.rust_version_cache.is_none() && self.current_dir.join("Cargo.toml").exists()
    }

    pub fn needs_node_check(&self) -> bool {
        self.node_version_cache.is_none()
            && (self.current_dir.join("package.json").exists()
                || self.current_dir.join("node_modules").exists())
    }
}

// Standalone functions (kept for async task compatibility)

pub async fn fetch_git_status_async(path: &Path) -> Option<GitStatus> {
    use tokio::process::Command;

    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("--no-optional-locks")
        .arg("status")
        .arg("--porcelain=2")
        .arg("--branch")
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    parse_git_status_output(&output.stdout)
}

fn parse_git_status_output(stdout: &[u8]) -> Option<GitStatus> {
    let mut status = GitStatus::new();
    let mut reader = BufReader::new(stdout);
    let mut buf = String::new();

    let mut modified = false;
    let mut untrack_file = false;

    while let Ok(size) = reader.read_line(&mut buf) {
        if size == 0 {
            break;
        }

        let splited: Vec<&str> = buf.split_whitespace().collect();

        if buf.starts_with('#') {
            if buf.starts_with("# branch.oid") {
                if let Some(oid) = splited.get(2) {
                    status.oid = Some(oid.to_string());
                }
            } else if buf.starts_with("# branch.head") {
                if let Some(branch) = splited.get(2) {
                    status.branch = branch.to_string();
                }
            } else if buf.starts_with("# branch.ab") {
                if let Some(val) = splited.get(2)
                    && let Ok(count) = val.replace('+', "").parse::<u32>()
                {
                    status.ahead = count;
                }
                if let Some(val) = splited.get(3)
                    && let Ok(count) = val.replace('-', "").parse::<u32>()
                {
                    status.behind = count;
                }
            }
        } else if buf.starts_with('1') {
            modified = true;
        } else if buf.starts_with('?') {
            untrack_file = true;
        }
        buf.clear();
    }

    let mut git_status = String::new();

    if modified {
        git_status += MODIFIED;
    }
    if untrack_file {
        git_status += UNTRACKED;
    }

    if !git_status.is_empty() {
        status.branch_status = Some(git_status);
    }

    if status.branch == "(detached)"
        && let Some(oid) = &status.oid
    {
        status.branch = oid.chars().take(8).collect();
    }

    Some(status)
}

pub async fn find_git_root_async(cwd: PathBuf) -> Option<PathBuf> {
    tokio::task::spawn_blocking(move || find_git_root(&cwd).map(PathBuf::from))
        .await
        .unwrap_or(None)
}

fn find_git_root(cwd: &Path) -> Option<String> {
    let mut p = cwd;
    loop {
        let git_dir = p.join(".git");
        if git_dir.exists() {
            if git_dir.is_dir() {
                return Some(p.to_string_lossy().into_owned());
            } else if git_dir.is_file() {
                if let Ok(content) = std::fs::read_to_string(&git_dir)
                    && content.trim().starts_with("gitdir:")
                {
                    return Some(p.to_string_lossy().into_owned());
                }
                break;
            }
        }
        if let Some(parent) = p.parent() {
            p = parent;
        } else {
            return None;
        }
    }

    let result = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(cwd)
        .output();

    if let Ok(output) = result
        && output.status.success()
        && let Ok(out) = String::from_utf8(output.stdout)
    {
        return Some(out.trim().to_string());
    }
    None
}

pub async fn fetch_rust_version_async() -> Option<String> {
    use tokio::process::Command;
    let output = Command::new("rustc").arg("--version").output().await.ok()?;

    if output.status.success() {
        // rustc 1.75.0 (82e1608df 2023-12-21)
        let out = String::from_utf8_lossy(&output.stdout);
        let version = out.split_whitespace().nth(1)?;
        Some(version.to_string())
    } else {
        None
    }
}

pub async fn fetch_node_version_async() -> Option<String> {
    use tokio::process::Command;
    let output = Command::new("node").arg("--version").output().await.ok()?;

    if output.status.success() {
        // v20.10.0
        let out = String::from_utf8_lossy(&output.stdout);
        Some(out.trim().to_string())
    } else {
        None
    }
}
