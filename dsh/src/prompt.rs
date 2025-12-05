use crate::environment::{ChangePwdHook, Environment};
use anyhow::Result;
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
const BRANCH_AHEAD: &str = "🦮";
const BRANCH_BEHIND: &str = "🐩";
const BRANCH_DIVERGED: &str = "🦖";
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
    current_git_root: Option<PathBuf>,
    git_root_cache: HashSet<String>,
    git_status_cache: Option<GitStatusCache>,
}

impl Prompt {
    pub fn new(current_dir: PathBuf, mark: String) -> Prompt {
        let mut prompt = Prompt {
            current_dir: current_dir.clone(),
            mark,
            current_git_root: None,
            git_root_cache: HashSet::new(),
            git_status_cache: None,
        };

        // Set Git root during initialization
        prompt.set_current(&current_dir);
        prompt
    }

    pub fn print_preprompt<W: Write>(&mut self, out: &mut W) {
        write!(out, "{}", "\r".reset()).ok();

        let (path, _is_git_root) = self.get_cwd();

        let has_git = self.under_git();
        // debug!(
        //     "print_preprompt: path={}, has_git={}, current_git_root={:?}",
        //     path, has_git, self.current_git_root
        // );

        if has_git {
            write!(out, "{}", path.cyan()).ok();

            if let Some(ref git_status) = self.get_git_status_cached() {
                // debug!(
                //     "Git status found: branch={}, status={:?}",
                //     git_status.branch, git_status.branch_status
                // );
                write!(out, " {} ", "on".reset()).ok();
                // Write branch mark and branch name separately to avoid format! allocation
                write!(out, "{}", BRANCH_MARK.magenta()).ok();
                write!(out, " ").ok();
                write!(out, "{}", git_status.branch.as_str().magenta()).ok();

                if let Some(status) = &git_status.branch_status {
                    write!(out, " [{}]", status.to_string().bold().red()).ok();
                }
                write!(out, "{}", "\r\n".reset()).ok();
            } else {
                // debug!("No git status found");
                write!(out, "{}", "\r\n".reset()).ok();
            }
        } else {
            // debug!("Not under git");
            write!(out, "{}", path.white()).ok();
            write!(out, "{}", "\r\n".reset()).ok();
        }
    }

    fn get_cwd(&self) -> (String, bool) {
        let pathbuf = &self.current_dir;
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

    fn under_git(&self) -> bool {
        if let Some(git_root) = &self.current_git_root {
            self.current_dir.starts_with(git_root)
        } else {
            false
        }
    }

    fn get_git_root(&self) -> Option<String> {
        for git_root in &self.git_root_cache {
            if self.current_dir.starts_with(git_root) {
                return Some(git_root.to_string());
            }
        }

        get_git_root()
    }

    pub fn current_path(&self) -> &Path {
        &self.current_dir
    }

    pub fn set_current(&mut self, path: &Path) {
        self.current_dir = path.to_path_buf();
        // debug!("set current dir {:?}", self.current_dir,);
        if let Some(git_root) = &self.current_git_root {
            let changed = !&self.current_dir.starts_with(git_root);
            // debug!("change_git_root: {:?}", changed);

            if changed {
                if let Some(root) = self.get_git_root() {
                    // debug!("Found new git root: {}", root);
                    self.current_git_root = Some(PathBuf::from(&root));
                    self.git_root_cache.insert(root);
                    // Git root changed, so clear cache
                    self.git_status_cache = None;
                } else {
                    // debug!("No git root found, clearing current_git_root");
                    self.current_git_root = None;
                }
            }
        } else if let Some(root) = self.get_git_root() {
            // debug!("Setting initial git root: {}", root);
            self.current_git_root = Some(PathBuf::from(&root));
            self.git_root_cache.insert(root);
            // New Git root, so clear cache
            self.git_status_cache = None;
        } else {
            // debug!("No git root found for initial setup");
        }
    }

    /// Get Git status with caching functionality
    pub fn get_git_status_cached(&mut self) -> Option<GitStatus> {
        // Early return if Git root doesn't exist
        let git_root = self.current_git_root.as_ref()?;

        // Check if cache is valid
        if let Some(ref cache) = self.git_status_cache
            && cache.is_valid(git_root)
        {
            // debug!("Using cached git status for {:?}", git_root);
            return Some(cache.status.clone());
        }

        // Get new status if cache is invalid or doesn't exist
        // debug!("Fetching fresh git status for {:?}", git_root);
        if let Some(status) = get_git_status() {
            // Create or update new cache
            if let Some(ref mut cache) = self.git_status_cache {
                cache.update(status.clone());
            } else {
                self.git_status_cache = Some(GitStatusCache::new(status.clone(), git_root.clone()));
            }
            Some(status)
        } else {
            // If Git status retrieval fails, clear cache
            self.git_status_cache = None;
            None
        }
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
            ttl: Duration::from_secs(10), // Cache valid for 10 seconds
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

    fn update(&mut self, status: GitStatus) {
        self.status = status;
        self.last_updated = Instant::now();
    }
}

fn get_git_root() -> Option<String> {
    // Optimization: Walk up looking for .git before spawning process
    if let Ok(cwd) = std::env::current_dir() {
        let mut p = cwd.as_path();
        let mut found = false;
        loop {
            if p.join(".git").exists() {
                found = true;
                break;
            }
            if let Some(parent) = p.parent() {
                p = parent;
            } else {
                break;
            }
        }
        if !found {
            return None;
        }
    }

    let result = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output();

    if let Ok(output) = result
        && output.status.success()
        && let Ok(out) = String::from_utf8(output.stdout)
    {
        return Some(out.trim().to_string());
    }
    None
}

fn get_git_status() -> Option<GitStatus> {
    // debug!("get_git_status: Starting git status check");
    let result = Command::new("git")
        .arg("-C")
        .arg(".")
        .arg("--no-optional-locks")
        .arg("status")
        .arg("--porcelain=2")
        .arg("--branch")
        .output();

    if let Ok(output) = result {
        // debug!(
        //     "get_git_status: Command executed, success={}",
        //     output.status.success()
        // );
        if output.status.success() {
            let mut status = GitStatus::new();
            let mut reader = BufReader::new(output.stdout.as_slice());
            let mut buf = String::new();

            let mut branch_status = String::new();
            let mut modified = false;
            let mut untrack_file = false;

            while let Ok(size) = reader.read_line(&mut buf) {
                if size == 0 {
                    break;
                }
                // debug!("get_git_status: Processing line: {}", buf.trim());

                let splited: Vec<&str> = buf.split_whitespace().collect();

                if buf.starts_with('#') {
                    // branch info
                    if buf.starts_with("# branch.head") {
                        if let Some(branch) = splited.get(2) {
                            status.branch = branch.to_string();
                            // debug!("get_git_status: Found branch: {}", status.branch);
                        }
                    } else if buf.starts_with("# branch.ab") {
                        if let Some(val) = splited.get(2)
                            && *val != "+0"
                        {
                            branch_status = BRANCH_AHEAD.to_string();
                        }
                        if let Some(val) = splited.get(3)
                            && *val != "-0"
                        {
                            if branch_status == BRANCH_AHEAD {
                                branch_status = BRANCH_DIVERGED.to_string();
                            } else {
                                branch_status = BRANCH_BEHIND.to_string();
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
            if !branch_status.is_empty() {
                git_status += branch_status.as_str();
            }

            if !git_status.is_empty() {
                status.branch_status = Some(git_status);
            }

            // debug!(
            //     "get_git_status: Final status - branch: {}, status: {:?}",
            //     status.branch, status.branch_status
            // );
            Some(status)
        } else {
            // debug!(
            //     "get_git_status: Command failed with status: {}",
            //     output.status
            // );
            None
        }
    } else {
        // debug!(
        //     "get_git_status: Failed to execute git command: {:?}",
        //     result
        // );
        None
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_git_status_cache() {
        // Test with non-Git directory
        let non_git_dir = PathBuf::from("/tmp");
        let mut prompt = Prompt::new(non_git_dir, "🐕 > ".to_string());

        // Explicitly clear Git root (may be auto-set in test environment)
        prompt.current_git_root = None;

        // Should return None when Git root is not set
        assert!(prompt.get_git_status_cached().is_none());

        // Test with actual Git repository
        if let Some(git_root) = get_git_root() {
            let git_dir = PathBuf::from(&git_root);
            let mut git_prompt = Prompt::new(git_dir, "🐕 > ".to_string());
            git_prompt.current_git_root = Some(PathBuf::from(&git_root));

            // First call (no cache)
            let status1 = git_prompt.get_git_status_cached();

            // Second call (with cache)
            let status2 = git_prompt.get_git_status_cached();

            // Confirm both results are the same
            assert_eq!(status1, status2);

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
}
