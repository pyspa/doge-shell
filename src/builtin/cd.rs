use crate::process::{Context, ExitStatus};
use crate::shell::Shell;
use dirs;
use std::fs::File;
use std::io::Write;
use std::mem;
use std::os::unix::io::FromRawFd;
use std::path::Path;

pub fn command(ctx: &Context, argv: Vec<String>, shell: &mut Shell) -> ExitStatus {
    let current_dir = std::env::current_dir().expect("failed to getcwd()");

    let dir = match argv.get(1).map(|s| s.as_str()) {
        Some(dir) if dir.starts_with('/') => dir.to_string(),
        Some(dir) if dir.starts_with('~') => shellexpand::tilde(dir).to_string(),
        Some(dir) => {
            let res = Path::new(&current_dir).join(dir.to_string()).canonicalize();

            match res {
                Ok(res) => res.to_string_lossy().into_owned(),
                Err(err) => {
                    let file = unsafe { File::from_raw_fd(ctx.outfile) };
                    writeln!(&file, "cd: {}: {}", err, dir).ok();
                    mem::forget(file);
                    return ExitStatus::ExitedWith(1);
                }
            }
        }
        None => {
            if let Some(home_dir) = dirs::home_dir() {
                home_dir.to_string_lossy().into_owned()
            } else {
                String::from("/")
            }
        }
    };

    match std::env::set_current_dir(&dir) {
        Ok(_) => {
            // save path
            if let Some(ref mut history) = shell.path_history {
                let _ = history.add(&dir);
            }
            ExitStatus::ExitedWith(0)
        }
        Err(err) => {
            let file = unsafe { File::from_raw_fd(ctx.outfile) };
            writeln!(&file, "cd: {}: {}", err, dir).ok();
            mem::forget(file);
            ExitStatus::ExitedWith(1)
        }
    }
}

pub fn move_dir(dir: &str, shell: &mut Shell) -> ExitStatus {
    let current_dir = std::env::current_dir().expect("failed to getcwd()");
    let dir = if dir.starts_with('/') {
        dir.to_string()
    } else if dir.starts_with("~") {
        shellexpand::tilde(dir).to_string()
    } else if dir == "" {
        if let Some(home_dir) = dirs::home_dir() {
            home_dir.to_string_lossy().into_owned()
        } else {
            String::from("/")
        }
    } else {
        Path::new(&current_dir)
            .join(dir.to_string())
            .canonicalize()
            .unwrap() // TODO error check
            .to_string_lossy()
            .into_owned()
    };
    match std::env::set_current_dir(&dir) {
        Ok(_) => {
            // save path
            if let Some(ref mut history) = shell.path_history {
                let _ = history.add(&dir);
            }
            ExitStatus::ExitedWith(0)
        }
        Err(err) => {
            eprint!("cd: {}: `{}'", err, dir);
            std::io::stderr().flush().ok();
            ExitStatus::ExitedWith(1)
        }
    }
}
