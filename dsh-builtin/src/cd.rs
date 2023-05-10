use super::ShellProxy;
use dirs;
use dsh_types::{Context, ExitStatus};
use std::fs::File;
use std::io::Write;
use std::mem;
use std::os::unix::io::FromRawFd;
use std::path::Path;

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let current_dir = std::env::current_dir().expect("failed to getcwd()");

    let dir = match argv.get(1).map(|s| s.as_str()) {
        Some(dir) if dir.starts_with('/') => dir.to_string(),
        Some(dir) if dir.starts_with('~') => shellexpand::tilde(dir).to_string(),
        Some(dir) => {
            let res = Path::new(&current_dir).join(dir).canonicalize();

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

    match proxy.changepwd(&dir) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(err) => {
            let file = unsafe { File::from_raw_fd(ctx.outfile) };
            writeln!(&file, "cd: {}: {}", err, dir).ok();
            mem::forget(file);
            ExitStatus::ExitedWith(1)
        }
    }
}
