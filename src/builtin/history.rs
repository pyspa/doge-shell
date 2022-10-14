use crate::frecency::SortMethod;
use crate::process::{Context, ExitStatus};
use crate::shell::Shell;
use nix::unistd::dup;
use std::fs::File;
use std::io::Write;
use std::os::unix::io::FromRawFd;

pub fn command(ctx: &Context, _argv: Vec<String>, shell: &mut Shell) -> ExitStatus {
    if let Some(ref mut history) = shell.cmd_history {
        let fd = dup(ctx.outfile).expect("failed dup");
        let mut file = unsafe { File::from_raw_fd(fd) };
        let vec = history.sorted(&SortMethod::Recent);
        for item in vec {
            writeln!(file, "{}", item.item).ok();
        }
        history.reset_index();
    }
    ExitStatus::ExitedWith(0)
}
