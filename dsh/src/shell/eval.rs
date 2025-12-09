use crate::parser::{self, Rule, ShellParser};
use crate::process::{Job, ListOp, ProcessState, wait_pid_job};
use crate::shell::{
    Shell,
    parse::{ParseContext, parse_commands},
    terminal::RawModeRestore,
};
use anyhow::{Context as _, Result, anyhow};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dsh_builtin::execute_chat_message;
use dsh_types::{Context, ExitStatus};
use nix::unistd::{ForkResult, Pid, fork, getpid, setpgid};
use pest::Parser;
use std::io::Write;
use std::sync::Arc;
use tokio::task;
use tracing::debug;

pub async fn eval_str(
    shell: &mut Shell,
    ctx: &mut Context,
    input: String,
    force_background: bool,
) -> Result<i32> {
    if ctx.save_history
        && let Some(ref mut history) = shell.cmd_history
    {
        let mut history = history.lock();
        history.add(&input);
        history.reset_index();
    }
    // TODO refactor context
    // let tmode = tcgetattr(0).expect("failed tcgetattr");

    if let Some(rest) = input.trim_start().strip_prefix('!') {
        disable_raw_mode().ok();
        let message = rest.trim_start();
        debug!(
            "AI_CHAT_EXEC: input='{}', extracted message='{}'",
            input, message
        );
        let status = execute_chat_message(ctx, shell, message, None);
        let code = match status {
            ExitStatus::ExitedWith(exit) if exit >= 0 => exit,
            ExitStatus::ExitedWith(_) => 1,
            ExitStatus::Running(_) => 0,
            ExitStatus::Break | ExitStatus::Continue | ExitStatus::Return => 0,
        };
        enable_raw_mode().ok();
        return Ok(code);
    }

    let jobs = get_jobs(shell, input.clone())?;
    let mut last_exit_code = 0;
    for mut job in jobs {
        // Execute pre-exec hooks
        if let Err(e) = shell.exec_pre_exec_hooks(&job.cmd) {
            debug!("Error executing pre-exec hooks: {}", e);
        }

        disable_raw_mode().ok();
        let mut raw_mode_guard = RawModeRestore::new();
        if force_background {
            // all job run background
            job.foreground = false;
        }

        job.job_id = shell.get_job_id(); // set job id

        debug!(
            "start job '{:?}' foreground:{:?} redirect:{:?} list_op:{:?} capture:{:?}",
            job.cmd, job.foreground, job.redirect, job.list_op, job.capture_output,
        );

        // Handle capture mode with |>
        if job.capture_output {
            let (exit, stdout, stderr) = execute_with_capture(shell, ctx, &job).await?;
            last_exit_code = exit as u8;

            // Save to output history
            {
                use crate::output_history::OutputEntry;
                let entry = OutputEntry::new(job.cmd.clone(), stdout.clone(), stderr.clone(), exit);
                shell.environment.write().output_history.push(entry);
                debug!(
                    "Captured output for '{}': {} bytes stdout, {} bytes stderr",
                    job.cmd,
                    stdout.len(),
                    stderr.len()
                );
            }

            // Also print to terminal
            if !stdout.is_empty() {
                print!("{}", stdout);
                std::io::stdout().flush().ok();
            }
            if !stderr.is_empty() {
                eprint!("{}", stderr);
                std::io::stderr().flush().ok();
            }

            raw_mode_guard.restore();
            continue;
        }

        let launch_result = job.launch(ctx, shell).await;
        let mut should_break = false;
        match launch_result {
            Ok(ProcessState::Running) => {
                debug!("job '{}' still running", job.cmd);
                shell.wait_jobs.push(job);
            }
            Ok(ProcessState::Stopped(pid, _signal)) => {
                debug!("job '{}' stopped pid: {:?}", job.cmd, pid);
                shell.wait_jobs.push(job);
            }
            Ok(ProcessState::Completed(exit, _signal)) => {
                debug!("job '{}' completed exit_code: {:?}", job.cmd, exit);
                last_exit_code = exit;

                // Execute post-exec hooks
                if let Err(e) = shell.exec_post_exec_hooks(&job.cmd, exit as i32) {
                    debug!("Error executing post-exec hooks: {}", e);
                }

                if job.list_op == ListOp::And && exit != 0 {
                    should_break = true;
                }
            }
            Err(err) => {
                ctx.pid = None;
                ctx.pgid = None;
                raw_mode_guard.restore();
                return Err(err);
            }
        }
        // reset
        ctx.pid = None;
        ctx.pgid = None;
        raw_mode_guard.restore();
        if should_break {
            break;
        }
    }

    enable_raw_mode().ok();
    Ok(last_exit_code as i32)
}

/// Execute a job and capture its stdout and stderr
/// Returns (exit_code, stdout, stderr)
async fn execute_with_capture(
    _shell: &mut Shell,
    _ctx: &mut Context,
    job: &Job,
) -> Result<(i32, String, String)> {
    use std::process::{Command, Stdio};

    // Strip the |> suffix from the command
    let cmd_str = job.cmd.trim();
    let cmd_str = cmd_str.strip_suffix("|>").unwrap_or(cmd_str).trim();

    debug!("Executing with capture: '{}'", cmd_str);

    // Use sh -c to execute the command
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd_str)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Failed to execute command: {}", cmd_str))?;

    let exit_code = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    debug!(
        "Capture complete: exit={}, stdout={} bytes, stderr={} bytes",
        exit_code,
        stdout.len(),
        stderr.len()
    );

    Ok((exit_code, stdout, stderr))
}

fn get_jobs(shell: &mut Shell, input: String) -> Result<Vec<Job>> {
    // TODO tests

    let (input_cow, pairs_opt) =
        parser::parse_with_expansion(&input, Arc::clone(&shell.environment))?;

    let mut pairs = if let Some(pairs) = pairs_opt {
        pairs
    } else {
        ShellParser::parse(Rule::commands, &input_cow).map_err(|e| anyhow!(e))?
    };

    let mut ctx = ParseContext::new(true);
    pairs.next().map_or_else(
        || Ok(Vec::new()),
        |pair| parse_commands(shell, &mut ctx, pair),
    )
}

pub fn launch_subshell(shell: &mut Shell, ctx: &mut Context, jobs: Vec<Job>) -> Result<()> {
    for mut job in jobs {
        disable_raw_mode().ok();
        let pid = task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(spawn_subshell(shell, ctx, &mut job))
        })?;
        debug!("spawned subshell cmd:{} pid: {:?}", job.cmd, pid);
        let res = wait_pid_job(pid, false);
        debug!("wait subshell exit:{:?}", res);
        enable_raw_mode().ok();
    }

    Ok(())
}

async fn spawn_subshell(shell: &mut Shell, ctx: &mut Context, job: &mut Job) -> Result<Pid> {
    let pid = unsafe { fork().context("failed fork")? };

    match pid {
        ForkResult::Parent { child } => {
            let pid = child;
            debug!("subshell parent setpgid parent pid:{} pgid:{}", pid, pid);
            setpgid(pid, pid).context("failed setpgid")?;
            Ok(pid)
        }
        ForkResult::Child => {
            let pid = getpid();
            debug!("subshell child setpgid pid:{} pgid:{}", pid, pid);
            setpgid(pid, pid).context("failed setpgid")?;

            job.pgid = Some(pid);
            ctx.pgid = Some(pid);
            debug!("subshell run context: {:?}", ctx);
            let res = job.launch(ctx, shell).await;
            debug!("subshell process '{}' exit:{:?}", job.cmd, res);

            if let Ok(ProcessState::Completed(exit, _)) = res {
                if exit != 0 {
                    // TODO check
                    debug!("job exit code {:?}", exit);
                }
                std::process::exit(i32::from(exit));
            } else {
                std::process::exit(-1);
            }
        }
    }
}
