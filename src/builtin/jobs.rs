use crate::process::{Context, ExitStatus};
use crate::shell::Shell;
use tabled::{Table, Tabled};

#[derive(Tabled)]
struct Job {
    job: usize,
    pid: i32,
    command: String,
}

pub fn command(_ctx: &Context, _argv: Vec<String>, shell: &mut Shell) -> ExitStatus {
    let jobs: Vec<Job> = shell
        .wait_jobs
        .iter()
        .map(|job| Job {
            job: job.job_id,
            pid: job.pid.as_raw(),
            command: job.cmd.clone(),
        })
        .collect();
    let table = Table::new(jobs).to_string();
    shell.print_stdout(table);
    ExitStatus::ExitedWith(0)
}
