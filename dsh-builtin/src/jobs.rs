use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // let jobs: Vec<Job> = shell
    //     .wait_jobs
    //     .iter()
    //     .map(|job| Job {
    //         job: job.job_id,
    //         pid: job.pid.as_raw(),
    //         command: job.cmd.clone(),
    //     })
    //     .collect();
    // let table = Table::new(jobs).to_string();
    // shell.print_stdout(table);

    proxy.dispatch(ctx, "jobs", argv).unwrap();
    ExitStatus::ExitedWith(0)
}
