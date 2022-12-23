use crate::process::{Context, ExitStatus};
use crate::shell::Shell;
use std::fs::File;
use std::io::prelude::*;
use std::os::unix::io::FromRawFd;

pub fn command(ctx: &Context, argv: Vec<String>, shell: &mut Shell) -> ExitStatus {
    let mut stdin = Vec::new();
    unsafe { File::from_raw_fd(ctx.infile).read_to_end(&mut stdin).ok() };
    let key = format!("${}", argv[1]);
    let output = std::str::from_utf8(&stdin)
        .map_err(|err| {
            // TODO
            eprintln!("{:?}", err);
            err
        })
        .unwrap()
        .trim_end_matches('\n')
        .to_owned();

    shell.environment.borrow_mut().variables.insert(key, output);
    ExitStatus::ExitedWith(0)
}
