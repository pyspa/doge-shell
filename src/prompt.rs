use crossterm::style::Stylize;
use std::process::Command;

pub const BRANCH_MARK: &'static str = "";

pub fn print_preprompt() {
    let (branch, git) = get_git_branch();

    let path = if git { get_cwd(false) } else { get_cwd(true) };

    print!("{}", "\r".reset());
    print!("{}", path.cyan());
    if git {
        print!(" {} ", "on".reset());
        print!("{}", format!("{} {}", BRANCH_MARK, branch).magenta());
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
