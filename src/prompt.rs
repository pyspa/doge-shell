use crossterm::style::Stylize;

use std::io::{BufRead, BufReader};
use std::process::Command;

// TODO stash, rename, delete

const BRANCH_MARK: &str = "🐾";
const BRANCH_AHEAD: &str = "🦮";
const BRANCH_BEHIND: &str = "🐩";
const BRANCH_DIVERGED: &str = "🦖";
const BRANCH_CONFLICT: &str = "🏴‍☠️";
const UNTRACKED: &str = "?";
const MODIFIED: &str = "!";
const NEW_FILE: &str = "+";

//
pub fn print_preprompt() {
    // let (branch, git) = get_git_branch();
    let status = get_git_status();
    let git = status.is_some();

    let path = if git { get_cwd(false) } else { get_cwd(true) };

    print!("{}", "\r".reset());
    print!("{}", path.white());
    if let Some(git_status) = status {
        print!(" {} ", "on".reset());
        print!(
            "{}",
            format!("{} {}", BRANCH_MARK, git_status.branch).magenta()
        );
        if git_status.branch_status.is_some() {
            print!(
                "{}",
                format!(" [{}]", git_status.branch_status.unwrap())
                    .bold()
                    .red()
            );
        }
        print!("{}", "\r\n".reset());
    } else {
        print!("{}", "\r\n".reset());
    }
}

fn get_cwd(full: bool) -> String {
    match std::env::current_dir() {
        Ok(pathbuf) => {
            if full {
                let path = pathbuf.display().to_string();
                let home = dirs::home_dir().map_or("".to_owned(), |p| p.display().to_string());
                path.replace(&home, "~")
            } else {
                pathbuf
                    .file_name()
                    .map_or("".to_owned(), |s| s.to_string_lossy().into_owned())
            }
        }
        Err(_) => String::from(""),
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
