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

// TODO stash, rename, delete

const BRANCH_MARK: &str = "🐾";
#[allow(dead_code)]
const BRANCH_CONFLICT: &str = "🏴‍☠️";
const UNTRACKED: &str = "?";
const MODIFIED: &str = "!";
#[allow(dead_code)]
const NEW_FILE: &str = "+";

impl ChangePwdHook for Arc<RwLock<Prompt>> {
    fn call(&self, pwd: &Path, _env: Arc<RwLock<Environment>>) -> Result<()> {
        // debug!("chpwd {:?}", pwd);
        self.write().set_current(pwd);
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Prompt {
    pub current_dir: PathBuf,
    pub mark: String,
    pub github_status: Option<Arc<RwLock<GitHubStatus>>>,
    pub github_icon: String,
    current_git_root: Option<PathBuf>,
    pub needs_git_check: bool,
    git_root_cache: HashSet<String>,
    git_status_cache: Option<GitStatusCache>,
}

impl Prompt {
    pub fn new(current_dir: PathBuf, mark: String) -> Prompt {
        let mut prompt = Prompt {
            current_dir: current_dir.clone(),
            mark,
            github_status: None,
            github_icon: "🐙".to_string(), // Default icon
            current_git_root: None,
            needs_git_check: true,
            git_root_cache: HashSet::new(),
            git_status_cache: None,
        };

        // Set Git root during initialization
        prompt.set_current(&current_dir);
        prompt
    }

    pub fn print_preprompt<W: Write>(&mut self, out: &mut W) {
        write!(out, "{}", "\r".reset()).ok();

        let (mut path, _is_git_root) = self.get_cwd();

        let has_git = self.under_git();
        // debug!(
        //     "print_preprompt: path={}, has_git={}, current_git_root={:?}",
        //     path, has_git, self.current_git_root
        // );

        let mut status_to_display = self.get_git_status_cached();
        let real_branch = self.get_head_branch();

        if has_git
            && let Some(ref real) = real_branch {
                let mut cache_invalid = false;
                if let Some(ref status) = status_to_display {
                    if status.branch != *real {
                        cache_invalid = true;
                    }
                } else {
                    cache_invalid = true;
                }

                if cache_invalid {
                    // Create a temporary status with the real branch name
                    let mut new_status = GitStatus::new();
                    new_status.branch = real.clone();

                    if status_to_display.is_some() {
                        // If we had a previous status but branch changed, keep the old status OID
                        // just in case, but clear ahead/behind/modified as they are likely wrong.
                        // Actually, cleaner to show nothing but branch name.
                    }

                    status_to_display = Some(new_status);

                    // Force invalidation of cache so async task picks it up
                    self.git_status_cache = None;
                    self.needs_git_check = true;
                }
            }

        if has_git && let Some(ref git_status) = status_to_display {
            // Ensure padding if we have prompt content
            // Write branch mark and branch name
            let branch_display = format!(
                " {} {} {}{}{}",
                "on".reset(),
                BRANCH_MARK.magenta(),
                git_status.branch.as_str().magenta(),
                // Ahead/Behind counts
                if git_status.ahead > 0 || git_status.behind > 0 {
                    let mut s = String::from(" ");
                    if git_status.ahead > 0 {
                        s.push_str(&format!("↑{}", git_status.ahead));
                        if git_status.behind > 0 {
                            s.push(' ');
                        }
                    }
                    if git_status.behind > 0 {
                        s.push_str(&format!("↓{}", git_status.behind));
                    }
                    s.cyan().to_string()
                } else {
                    "".to_string()
                },
                if let Some(status) = &git_status.branch_status {
                    format!(" [{}]", status.to_string().bold().red())
                } else {
                    "".to_string()
                }
            );

            path.push_str(&branch_display);

            // Display GitHub notifications if available and under git
            if let Some(status_lock) = &self.github_status {
                let status = status_lock.read();
                if status.notification_count > 0 {
                    let notify_display = format!(
                        " {} {}",
                        self.github_icon.as_str().yellow(),
                        status.notification_count.to_string().yellow().bold()
                    );
                    path.push_str(&notify_display);
                } else if status.has_error {
                    let notify_display = format!(" {}", "🔔!".red().bold());
                    path.push_str(&notify_display);
                }
            }
        }

        write!(out, "{}", path).ok();
    }

    pub fn print_right_prompt<W: Write>(
        &self,
        out: &mut W,
        cols: usize,
        last_status: i32,
        last_duration: Option<Duration>,
    ) {
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

        // Construct the full right prompt
        let right_prompt = format!("{}{}{}", status_str, duration_str, time_str.as_str().dim());

        // Calculate visible length (approximate, ignoring ANSI for simplistic calc or using crate::input::display_width if available)
        // Since we are in prompt.rs, we might not have easy access to input::display_width readily available as public.
        // Let's use string len for now but strip ansi if possible, or just assume minimal ansi impact for simple case.
        // Better: Use `crossterm` functionality or simple approximation.
        // For accurate alignment, we need visual width.
        // Attempting to calculate visual width by stripping ANSI codes manually or importing input::display_width if accessible.
        // crate::input::display_width IS public.

        let right_width = crate::input::display_width(&right_prompt);

        if cols > right_width + 1 {
            let start_col = cols - right_width - 1;
            queue!(
                out,
                cursor::MoveToColumn(start_col as u16),
                crossterm::style::Print(right_prompt),
                cursor::MoveToColumn(0) // Reset to start for preprompt which prints \r anyway
            )
            .ok();
        }
    }

    fn get_cwd(&self) -> (String, bool) {
        let (path_str, is_git_context) = format_prompt_path(
            &self.current_dir,
            self.current_git_root.as_deref(),
            dirs::home_dir().as_deref(),
        );

        if is_git_context {
            (path_str.cyan().to_string(), true)
        } else {
            (path_str, false)
        }
    }

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
        // debug!("set current dir {:?}", self.current_dir,);

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
                // debug!("Found cached git root: {}", root);
                self.current_git_root = Some(PathBuf::from(&root));
                self.git_status_cache = None;
                self.needs_git_check = false;
            } else {
                // Cache miss, need to check (async)
                // debug!("No cached git root found, scheduling check");
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
        // Status cache is invalid if root changed (handled by is_valid check or just clear)
        self.git_status_cache = None;
    }

    /// Get Git status with caching functionality
    /// This method is now non-blocking and returns only cached values.
    /// Returns None if cache is empty or expired/invalid, but does NOT trigger a refresh.
    /// Refresh should be triggered asynchronously using fetch_git_status_async and update_git_status.
    pub fn get_git_status_cached(&self) -> Option<GitStatus> {
        let git_root = self.current_git_root.as_ref()?;
        let cache = self.git_status_cache.as_ref()?;

        if cache.is_valid(git_root) {
            Some(cache.status.clone())
        } else {
            // Return stale data if available?
            // For now, let's return the stale data but we rely on the caller to check should_refresh
            // Actually, if it's invalid (different root), we shouldn't return it.
            // If it's just expired, we might want to return it while updating.
            // Current is_valid checks both.

            // Let's rely on should_refresh to trigger updates, but here we be conservative.
            // If strictly invalid (root mismatch), return None.
            if cache.git_root != *git_root {
                return None;
            }
            // If just expired, return stale data
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
        } else {
            // Failed to get status or not a git repo anymore?
            // If we are sure it's a git repo but failed, maybe keep old cache?
            // For now do nothing or clear?
        }
    }

    fn get_head_branch(&self) -> Option<String> {
        let git_root = self.current_git_root.as_ref()?;
        let git_dir = git_root.join(".git");

        // Resolve .git file (worktree/submodule) if necessary
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
                // Detached HEAD or other state
                if content.len() >= 7 {
                    return Some(content[..7].to_string());
                }
                return Some("DETACHED".to_string());
            }
        }
        None
    }
}

#[allow(dead_code)]
fn get_cwd() -> (String, bool) {
    match std::env::current_dir() {
        Ok(pathbuf) => {
            let is_git_root = pathbuf.join(".git").exists();

            let path = if is_git_root {
                pathbuf
                    .file_name()
                    .map_or("".to_owned(), |s| s.to_string_lossy().into_owned())
            } else {
                let path = pathbuf.display().to_string();
                let home = dirs::home_dir().map_or("".to_owned(), |p| p.display().to_string());
                path.replace(&home, "~")
            };
            (path, is_git_root)
        }
        Err(_) => (String::from(""), false),
    }
}

#[allow(dead_code)]
fn get_git_branch() -> (String, bool) {
    let result = Command::new("git")
        .arg("symbolic-ref")
        .arg("--short")
        .arg("HEAD")
        .output();

    match result {
        Ok(output) => {
            let result = ("".to_owned(), false);
            if output.status.success() {
                if let Ok(out) = String::from_utf8(output.stdout) {
                    (out.trim().to_string(), true)
                } else {
                    result
                }
            } else {
                result
            }
        }
        Err(_) => ("".to_owned(), false),
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

pub async fn fetch_git_status_async(path: &Path) -> Option<GitStatus> {
    // using tokio::process::Command for non-blocking execution
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

    // Parse output (logic shared with get_git_status, could refactor to shared parser)
    // For now, duplicate or extract parser.
    parse_git_status_output(&output.stdout)
}

fn parse_git_status_output(stdout: &[u8]) -> Option<GitStatus> {
    let mut status = GitStatus::new();
    let mut reader = BufReader::new(stdout);
    let mut buf = String::new();

    // let branch_status = String::new();
    let mut modified = false;
    let mut untrack_file = false;

    while let Ok(size) = reader.read_line(&mut buf) {
        if size == 0 {
            break;
        }

        let splited: Vec<&str> = buf.split_whitespace().collect();

        if buf.starts_with('#') {
            // branch info
            if buf.starts_with("# branch.oid") {
                if let Some(oid) = splited.get(2) {
                    status.oid = Some(oid.to_string());
                }
            } else if buf.starts_with("# branch.head") {
                if let Some(branch) = splited.get(2) {
                    status.branch = branch.to_string();
                }
            } else if buf.starts_with("# branch.ab") {
                if let Some(val) = splited.get(2) {
                    // +0, +1
                    if let Ok(count) = val.replace('+', "").parse::<u32>() {
                        status.ahead = count;
                    }
                }
                if let Some(val) = splited.get(3) {
                    // -0, -1
                    if let Ok(count) = val.replace('-', "").parse::<u32>() {
                        status.behind = count;
                    }
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

    // Note: Ahead/Behind are stored in structured fields and displayed by print_preprompt
    // We do NOT add them to git_status string here to avoid duplication

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
    // Optimization: Walk up looking for .git before spawning process
    // This part is safe to run in thread pool
    tokio::task::spawn_blocking(move || find_git_root(&cwd).map(PathBuf::from))
        .await
        .unwrap_or(None)
}

fn find_git_root(cwd: &Path) -> Option<String> {
    // Optimization: Walk up looking for .git before spawning process
    let mut p = cwd;
    loop {
        let git_dir = p.join(".git");
        if git_dir.exists() {
            if git_dir.is_dir() {
                return Some(p.to_string_lossy().into_owned());
            } else if git_dir.is_file() {
                // Check if it's a gitdir pointer (worktree or submodule)
                // Content format: "gitdir: <path>\n"
                if let Ok(content) = std::fs::read_to_string(&git_dir)
                    && content.trim().starts_with("gitdir:")
                {
                    return Some(p.to_string_lossy().into_owned());
                }
                // If exists but not a directory and doesn't look like a git pointer,
                // we fall back to git rev-parse to be safe.
                break;
            }
        }
        if let Some(parent) = p.parent() {
            p = parent;
        } else {
            // Reached root without finding .git
            return None;
        }
    }

    let result = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(cwd) // Ensure we run from the correct directory
        .output();

    if let Ok(output) = result
        && output.status.success()
        && let Ok(out) = String::from_utf8(output.stdout)
    {
        return Some(out.trim().to_string());
    }
    None
}

/// Helper function to format the path for the prompt
/// Returns: (formatted_path, is_git_context)
fn format_prompt_path(
    current_dir: &Path,
    git_root: Option<&Path>,
    home_dir: Option<&Path>,
) -> (String, bool) {
    if let Some(git_root) = git_root
        && current_dir.starts_with(git_root)
    {
        // Under git: show relative path from git root
        let root_name = git_root
            .file_name()
            .map_or("".to_string(), |s| s.to_string_lossy().to_string());

        let relative_path = current_dir.strip_prefix(git_root).unwrap_or(current_dir);

        let path_str = if relative_path.as_os_str().is_empty() {
            root_name
        } else {
            format!("{}/{}", root_name, relative_path.display())
        };

        return (path_str, true);
    }

    let is_git_root = current_dir.join(".git").exists();
    if is_git_root {
        let path = current_dir
            .file_name()
            .map_or("".to_owned(), |s| s.to_string_lossy().into_owned());
        (path, false)
    } else {
        let path = current_dir.display().to_string();
        if let Some(home) = home_dir {
            let home_str = home.display().to_string();
            (path.replace(&home_str, "~"), false)
        } else {
            (path, false)
        }
    }
}

#[cfg(test)]
mod prompt_path_tests {
    use super::*;

    #[test]
    fn test_format_prompt_path_standard() {
        let current = PathBuf::from("/home/user/docs");
        let home = PathBuf::from("/home/user");
        let (path, is_git) = format_prompt_path(&current, None, Some(&home));
        assert_eq!(path, "~/docs");
        assert!(!is_git);
    }

    #[test]
    fn test_format_prompt_path_git_root() {
        let current = PathBuf::from("/home/user/repo");
        let git_root = PathBuf::from("/home/user/repo");
        let home = PathBuf::from("/home/user");

        let (path, is_git) = format_prompt_path(&current, Some(&git_root), Some(&home));
        assert_eq!(path, "repo");
        assert!(is_git);
    }

    #[test]
    fn test_format_prompt_path_git_subdir() {
        let current = PathBuf::from("/home/user/repo/src");
        let git_root = PathBuf::from("/home/user/repo");
        let home = PathBuf::from("/home/user");

        let (path, is_git) = format_prompt_path(&current, Some(&git_root), Some(&home));
        assert_eq!(path, "repo/src");
        assert!(is_git);
    }

    #[test]
    fn test_format_prompt_path_no_home() {
        let current = PathBuf::from("/tmp/test");
        let (path, is_git) = format_prompt_path(&current, None, None);
        assert_eq!(path, "/tmp/test");
        assert!(!is_git);
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[tokio::test]
    async fn test_git_status_cache() {
        // Test with non-Git directory
        let non_git_dir = PathBuf::from("/tmp");
        let mut prompt = Prompt::new(non_git_dir, "🐕 > ".to_string());

        // Explicitly clear Git root (may be auto-set in test environment)
        prompt.current_git_root = None;

        // Should return None when Git root is not set
        assert!(prompt.get_git_status_cached().is_none());

        // Test with actual Git repository
        if let Some(git_root) = find_git_root(&std::env::current_dir().unwrap()) {
            let git_dir = PathBuf::from(&git_root);
            let mut git_prompt = Prompt::new(git_dir, "🐕 > ".to_string());
            git_prompt.current_git_root = Some(PathBuf::from(&git_root));

            // First call (no cache)
            assert!(git_prompt.get_git_status_cached().is_none());

            // Populate cache manually (mimic async fetch)
            if let Some(status) = fetch_git_status_async(&PathBuf::from(&git_root)).await {
                git_prompt.update_git_status(Some(status));
            }

            // Second call (with cache)
            let status2 = git_prompt.get_git_status_cached();
            assert!(status2.is_some());

            // Confirm cache exists
            assert!(git_prompt.git_status_cache.is_some());
        }
    }

    #[test]
    fn test_git_status_cache_validity() {
        let git_root = PathBuf::from("/tmp");
        let status = GitStatus::new();
        let cache = GitStatusCache::new(status, git_root.clone());

        // Valid for same Git root
        assert!(cache.is_valid(&git_root));

        // Invalid for different Git root
        let different_root = PathBuf::from("/home");
        assert!(!cache.is_valid(&different_root));
    }

    #[test]
    fn test_git_status_cache_ttl() {
        use std::thread;

        let git_root = PathBuf::from("/tmp");
        let status = GitStatus::new();
        let mut cache = GitStatusCache::new(status, git_root.clone());

        // Set short TTL
        cache.ttl = Duration::from_millis(10);

        // Initially valid
        assert!(cache.is_valid(&git_root));

        // Wait until TTL expires
        thread::sleep(Duration::from_millis(20));

        // Invalid after TTL expiration
        assert!(!cache.is_valid(&git_root));
    }

    #[test]
    fn test_parse_git_status_output() {
        let output = b"# branch.oid 1234567890\n# branch.head main\n# branch.ab +1 -2\n1 .M N... 100644 100644 100644 123456 123456 file.txt\n? untracked.txt\n";

        let status = parse_git_status_output(output).expect("Should parse successfully");

        assert_eq!(status.branch, "main");
        assert_eq!(status.ahead, 1);
        assert_eq!(status.behind, 2);

        // Check branch_status contains ONLY modified/untracked, NOT arrows
        let branch_status = status.branch_status.unwrap();
        assert!(!branch_status.contains("↑"));
        assert!(!branch_status.contains("↓"));
        assert!(branch_status.contains("!")); // Modified
        assert!(branch_status.contains("?")); // Untracked
    }

    #[test]
    fn test_parse_git_status_output_detached() {
        let output =
            b"# branch.oid 1234567890abcdef\n# branch.head (detached)\n# branch.ab +0 -0\n";
        let status = parse_git_status_output(output).expect("Should parse successfully");

        assert_eq!(status.branch, "12345678");
        assert_eq!(status.oid, Some("1234567890abcdef".to_string()));
    }

    #[test]
    fn test_get_git_root_with_file() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::tempdir;

        // Create a temp directory simulating a worktree root
        let dir = tempdir().expect("failed into create temp dir");
        let dir_path = dir.path();

        // Create a .git file
        let git_file_path = dir_path.join(".git");
        let mut file = File::create(git_file_path).expect("failed to create .git file");
        writeln!(file, "gitdir: /path/to/repo.git/worktrees/my-worktree")
            .expect("failed to write to .git file");

        // Use the new exposed helper
        let root = find_git_root(dir_path);

        assert_eq!(root, Some(dir_path.to_string_lossy().into_owned()));
    }

    #[test]
    fn test_get_head_branch() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::tempdir;

        let dir = tempdir().expect("failed into create temp dir");
        let dir_path = dir.path();

        // Setup .git/HEAD
        let git_dir = dir_path.join(".git");
        std::fs::create_dir(&git_dir).expect("failed to create .git dir");
        let head_path = git_dir.join("HEAD");

        // Case 1: Standard branch ref
        {
            let mut file = File::create(&head_path).expect("failed to create HEAD");
            writeln!(file, "ref: refs/heads/main").expect("failed to write HEAD");

            let mut prompt = Prompt::new(dir_path.to_path_buf(), "🐕 > ".to_string());
            prompt.update_git_root(Some(dir_path.to_path_buf()));

            assert_eq!(prompt.get_head_branch(), Some("main".to_string()));
        }

        // Case 2: Changed branch ref
        {
            let mut file = File::create(&head_path).expect("failed to create HEAD");
            writeln!(file, "ref: refs/heads/feature/test").expect("failed to write HEAD");

            let mut prompt = Prompt::new(dir_path.to_path_buf(), "🐕 > ".to_string());
            prompt.update_git_root(Some(dir_path.to_path_buf()));

            assert_eq!(prompt.get_head_branch(), Some("feature/test".to_string()));
        }

        // Case 3: Detached HEAD (SHA)
        {
            let mut file = File::create(&head_path).expect("failed to create HEAD");
            writeln!(file, "a1b2c3d4e5f6g7h8i9j0").expect("failed to write HEAD");

            let mut prompt = Prompt::new(dir_path.to_path_buf(), "🐕 > ".to_string());
            prompt.update_git_root(Some(dir_path.to_path_buf()));

            assert_eq!(prompt.get_head_branch(), Some("a1b2c3d".to_string()));
        }
    }
}
