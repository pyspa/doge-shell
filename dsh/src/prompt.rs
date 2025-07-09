use crate::environment::{ChangePwdHook, Environment};
use anyhow::Result;
use crossterm::style::Stylize;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, BufWriter, StdoutLock, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::debug;

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
        debug!("chpwd {:?}", pwd);
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
        Prompt {
            current_dir,
            mark,
            current_git_root: None,
            git_root_cache: HashSet::new(),
            git_status_cache: None,
        }
    }

    pub fn print_preprompt(&mut self, out: &mut StdoutLock<'static>) {
        let mut out = BufWriter::new(out);
        out.write_fmt(format_args!("{}", "\r".reset())).ok();

        let (path, _is_git_root) = self.get_cwd();

        let has_git = self.under_git();

        if has_git {
            out.write_fmt(format_args!("{}", path.cyan())).ok();

            if let Some(ref git_status) = self.get_git_status_cached() {
                out.write_fmt(format_args!(" {} ", "on".reset())).ok();
                out.write_fmt(format_args!(
                    "{}",
                    format!("{} {}", BRANCH_MARK, git_status.branch).magenta(),
                ))
                .ok();

                if let Some(status) = &git_status.branch_status {
                    out.write_fmt(format_args!(" [{}]", status.to_string().bold().red()))
                        .ok();
                }
                out.write_fmt(format_args!("{}", "\r\n".reset(),)).ok();
            } else {
                out.write_fmt(format_args!("{}", "\r\n".reset(),)).ok();
            }
        } else {
            out.write_fmt(format_args!("{}", path.white())).ok();
            out.write_fmt(format_args!("{}", "\r\n".reset(),)).ok();
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

    pub fn set_current(&mut self, path: &Path) {
        self.current_dir = path.to_path_buf();
        debug!("set current dir {:?}", self.current_dir,);
        if let Some(git_root) = &self.current_git_root {
            let changed = !&self.current_dir.starts_with(git_root);
            debug!("change_git_root: {:?}", changed);

            if changed {
                if let Some(root) = self.get_git_root() {
                    self.current_git_root = Some(PathBuf::from(&root));
                    self.git_root_cache.insert(root);
                    // Git rootが変わったのでキャッシュをクリア
                    self.git_status_cache = None;
                }
            }
        } else if let Some(root) = self.get_git_root() {
            self.current_git_root = Some(PathBuf::from(&root));
            self.git_root_cache.insert(root);
            // 新しいGit rootなのでキャッシュをクリア
            self.git_status_cache = None;
        }
    }

    /// キャッシュ機能付きのGit状態取得
    pub fn get_git_status_cached(&mut self) -> Option<GitStatus> {
        // Git rootが存在しない場合は早期リターン
        let git_root = self.current_git_root.as_ref()?;

        // キャッシュが有効かチェック
        if let Some(ref cache) = self.git_status_cache {
            if cache.is_valid(git_root) {
                debug!("Using cached git status for {:?}", git_root);
                return Some(cache.status.clone());
            }
        }

        // キャッシュが無効または存在しない場合、新しい状態を取得
        debug!("Fetching fresh git status for {:?}", git_root);
        if let Some(status) = get_git_status() {
            // 新しいキャッシュを作成または更新
            if let Some(ref mut cache) = self.git_status_cache {
                cache.update(status.clone());
            } else {
                self.git_status_cache = Some(GitStatusCache::new(status.clone(), git_root.clone()));
            }
            Some(status)
        } else {
            // Git状態の取得に失敗した場合、キャッシュをクリア
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

/// Git状態のキャッシュ構造体
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
            ttl: Duration::from_secs(5), // 5秒間キャッシュを有効とする
        }
    }

    fn is_valid(&self, current_git_root: &Path) -> bool {
        // Git rootが変わった場合は無効
        if self.git_root != current_git_root {
            return false;
        }

        // TTLを超えた場合は無効
        self.last_updated.elapsed() < self.ttl
    }

    fn update(&mut self, status: GitStatus) {
        self.status = status;
        self.last_updated = Instant::now();
    }
}

fn get_git_root() -> Option<String> {
    let result = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output();

    if let Ok(output) = result {
        if output.status.success() {
            let out = String::from_utf8(output.stdout).expect("failed");
            return Some(out.trim().to_string());
        }
    }
    None
}

fn get_git_status() -> Option<GitStatus> {
    let result = Command::new("git")
        .arg("-C")
        .arg(".")
        .arg("--no-optional-locks")
        .arg("status")
        .arg("--porcelain=2")
        .arg("--branch")
        .output();

    if let Ok(output) = result {
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

                let splited: Vec<&str> = buf.split_whitespace().collect();

                if buf.starts_with('#') {
                    // branch info
                    if buf.starts_with("# branch.head") {
                        if let Some(branch) = splited.get(2) {
                            status.branch = branch.to_string();
                        }
                    } else if buf.starts_with("# branch.ab") {
                        if let Some(val) = splited.get(2) {
                            if *val != "+0" {
                                branch_status = BRANCH_AHEAD.to_string();
                            }
                        }
                        if let Some(val) = splited.get(3) {
                            if *val != "-0" {
                                if branch_status == BRANCH_AHEAD {
                                    branch_status = BRANCH_DIVERGED.to_string();
                                } else {
                                    branch_status = BRANCH_BEHIND.to_string();
                                }
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

            Some(status)
        } else {
            None
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_git_status_cache() {
        let current_dir = PathBuf::from("/tmp");
        let mut prompt = Prompt::new(current_dir, "🐕 > ".to_string());

        // Git rootが設定されていない場合はNoneを返す
        assert!(prompt.get_git_status_cached().is_none());

        // Git rootを設定
        if let Some(git_root) = get_git_root() {
            prompt.current_git_root = Some(PathBuf::from(&git_root));

            // 初回呼び出し（キャッシュなし）
            let status1 = prompt.get_git_status_cached();

            // 2回目呼び出し（キャッシュあり）
            let status2 = prompt.get_git_status_cached();

            // 両方とも同じ結果であることを確認
            assert_eq!(status1, status2);

            // キャッシュが存在することを確認
            assert!(prompt.git_status_cache.is_some());
        }
    }

    #[test]
    fn test_git_status_cache_validity() {
        let git_root = PathBuf::from("/tmp");
        let status = GitStatus::new();
        let cache = GitStatusCache::new(status, git_root.clone());

        // 同じGit rootの場合は有効
        assert!(cache.is_valid(&git_root));

        // 異なるGit rootの場合は無効
        let different_root = PathBuf::from("/home");
        assert!(!cache.is_valid(&different_root));
    }

    #[test]
    fn test_git_status_cache_ttl() {
        use std::thread;

        let git_root = PathBuf::from("/tmp");
        let status = GitStatus::new();
        let mut cache = GitStatusCache::new(status, git_root.clone());

        // TTLを短く設定
        cache.ttl = Duration::from_millis(10);

        // 初期状態では有効
        assert!(cache.is_valid(&git_root));

        // TTLを超えるまで待機
        thread::sleep(Duration::from_millis(20));

        // TTL超過後は無効
        assert!(!cache.is_valid(&git_root));
    }
}
