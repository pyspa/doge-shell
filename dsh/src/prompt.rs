use crate::environment::{ChangePwdHook, Environment};
use anyhow::Result;
use crossterm::style::Stylize;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, BufWriter, StdoutLock, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{cell::RefCell, rc::Rc};
use tracing::debug;

// TODO stash, rename, delete

const BRANCH_MARK: &str = "🐾";
const BRANCH_AHEAD: &str = "🦮";
const BRANCH_BEHIND: &str = "🐩";
const BRANCH_DIVERGED: &str = "🦖";
const BRANCH_CONFLICT: &str = "🏴‍☠️";
const UNTRACKED: &str = "?";
const MODIFIED: &str = "!";
const NEW_FILE: &str = "+";

impl ChangePwdHook for std::rc::Rc<RefCell<Prompt>> {
    fn call(&self, pwd: &Path, _env: Rc<RefCell<Environment>>) -> Result<()> {
        debug!("chpwd {:?}", pwd);
        self.borrow_mut().set_current(pwd);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub current_dir: PathBuf,
    pub mark: String,
    current_git_root: Option<PathBuf>,
    git_root_cache: HashSet<String>,
}

impl Prompt {
    pub fn new(current_dir: PathBuf, mark: String) -> Prompt {
        Prompt {
            current_dir,
            mark,
            current_git_root: None,
            git_root_cache: HashSet::new(),
        }
    }

    pub fn print_preprompt(&self, out: &mut StdoutLock<'static>) {
        let mut out = BufWriter::new(out);
        out.write_fmt(format_args!("{}", "\r".reset())).ok();

        let (path, _is_git_root) = self.get_cwd();

        let has_git = self.under_git();

        if has_git {
            out.write_fmt(format_args!("{}", path.cyan())).ok();

            if let Some(ref git_status) = get_git_status() {
                out.write_fmt(format_args!(" {} ", "on".reset())).ok();
                out.write_fmt(format_args!(
                    "{}",
                    format!("{} {}", BRANCH_MARK, git_status.branch).magenta(),
                ))
                .ok();

                if let Some(ref status) = &git_status.branch_status {
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
                }
            }
        } else if let Some(root) = self.get_git_root() {
            self.current_git_root = Some(PathBuf::from(&root));
            self.git_root_cache.insert(root);
        }
    }
}

//
fn print_preprompt(out: &mut StdoutLock<'static>) {
    let status = get_git_status();
    let has_git = status.is_some();

    let (path, _is_git_root) = get_cwd();

    let mut out = BufWriter::new(out);
    out.write_fmt(format_args!("{}", "\r".reset())).ok();

    if has_git {
        out.write_fmt(format_args!("{}", path.cyan())).ok();
    } else {
        out.write_fmt(format_args!("{}", path.white())).ok();
    }

    if let Some(git_status) = status {
        out.write_fmt(format_args!(" {} ", "on".reset())).ok();
        out.write_fmt(format_args!(
            "{}",
            format!("{} {}", BRANCH_MARK, git_status.branch).magenta(),
        ))
        .ok();
        if git_status.branch_status.is_some() {
            out.write_fmt(format_args!(
                "{}",
                format!(" [{}]", git_status.branch_status.unwrap())
                    .bold()
                    .red()
            ))
            .ok();
        }
        out.write_fmt(format_args!("{}", "\r\n".reset(),)).ok();
    } else {
        out.write_fmt(format_args!("{}", "\r\n".reset(),)).ok();
    }
}

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

impl GitStatus {
    pub fn new() -> Self {
        GitStatus {
            branch: "".to_string(),
            branch_status: None,
        }
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
                    } else {
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

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn test_new() {
        init();
        let curr = PathBuf::from("~/");
        let prompt = Prompt::new(curr, "$".to_string());
        let (root, git) = prompt.get_cwd();
        assert_eq!("~/", root);
        assert!(!git);
    }

    #[test]
    fn test_set_current() {
        init();
        let curr = PathBuf::from("~/");
        let mut prompt = Prompt::new(curr, "$".to_string());
        let git_root = PathBuf::from("~/");
        prompt.current_git_root = Some(git_root);

        let new_curr = PathBuf::from("./");
        prompt.set_current(new_curr.as_path());
    }
}
