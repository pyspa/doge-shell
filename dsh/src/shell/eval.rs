use crate::parser::{self, Rule, ShellParser};
use crate::process::{Job, ListOp, ProcessState, wait_pid_job};
use crate::shell::{
    Shell,
    parse::{ParseContext, parse_commands},
};
use anyhow::{Context as _, Result, anyhow};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dsh_builtin::execute_chat_message;
use dsh_types::{Context, ExitStatus};
use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
use nix::unistd::{ForkResult, Pid, fork, getpid, setpgid};
use pest::Parser;
use std::io::Write;
use std::os::unix::io::AsRawFd;
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
        if let Err(e) = history.write_history(&input) {
            debug!("Failed to write history: {}", e);
        }
    }

    if let Some(rest) = input.trim_start().strip_prefix('!') {
        if let Err(e) = disable_raw_mode() {
            tracing::error!("Failed to disable raw mode: {}", e);
        } else {
            tracing::info!("Raw mode disabled successfully");
        }

        // Force enable ISIG to ensure Ctrl+C generates SIGINT
        // This addresses issues where crossterm might not fully restore terminal flags
        if let Ok(mut termios) = tcgetattr(std::io::stdin().as_raw_fd())
            && !termios.local_flags.contains(LocalFlags::ISIG)
        {
            termios.local_flags.insert(LocalFlags::ISIG);
            if let Err(e) = tcsetattr(std::io::stdin().as_raw_fd(), SetArg::TCSANOW, &termios) {
                tracing::error!("Failed to force enable ISIG: {}", e);
            }
        }

        // Ensure signals are set correctly before AI execution
        shell.set_signals();

        let message = rest.trim_start();
        debug!(
            "AI_CHAT_EXEC: input_len={}, message_len={}",
            input.len(),
            message.len()
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

    // Smart Pipe transformation
    let input = transform_input_for_smart_pipe(input);

    let jobs = get_jobs(shell, &input)?;
    let mut last_exit_code = 0_i32;
    // Operator that gates execution of the *current* job based on the previous job result.
    // This is effectively "the separator between previous and current job".
    let mut gate_op = ListOp::None;
    for mut job in jobs {
        // `list_op` is stored on the *previous* job by the parser.
        // We keep it here before moving `job` into wait_jobs.
        let next_gate_op = job.list_op.clone();

        // Decide whether to run this job based on previous operator and last exit code.
        let should_run = match gate_op {
            ListOp::None => true,
            ListOp::And => last_exit_code == 0,
            ListOp::Or => last_exit_code != 0,
        };

        if !should_run {
            debug!(
                "skip job '{}' due to gate_op:{:?} last_exit_code:{}",
                job.cmd, gate_op, last_exit_code
            );
            gate_op = next_gate_op;
            continue;
        }

        // Execute pre-exec hooks
        if let Err(e) = shell.exec_pre_exec_hooks(&job.cmd) {
            debug!("Error executing pre-exec hooks: {}", e);
        }

        // Disable raw mode for command execution (cooked mode allows proper newline handling)
        if let Err(e) = disable_raw_mode() {
            debug!("EVAL_STR: Failed to disable raw mode: {}", e);
        } else {
            debug!("EVAL_STR: Successfully disabled raw mode");
        }

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
            last_exit_code = exit;

            // Save to output history
            {
                use dsh_types::output_history::OutputEntry;
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

            // Re-enable raw mode after capture job
            enable_raw_mode().ok();
            gate_op = next_gate_op;
            continue;
        }

        let launch_result = job.launch(ctx, shell).await;
        let mut stop_processing = false;
        match launch_result {
            Ok(ProcessState::Running) => {
                debug!("job '{}' still running", job.cmd);
                shell.wait_jobs.push(job);
                // Background jobs are considered successfully started.
                last_exit_code = 0;
            }
            Ok(ProcessState::Stopped(pid, _signal)) => {
                debug!("job '{}' stopped pid: {:?}", job.cmd, pid);
                shell.wait_jobs.push(job);
                // If a job is stopped, we return control to the user and do not continue
                // evaluating the rest of the command list.
                stop_processing = true;
            }
            Ok(ProcessState::Completed(exit, _signal)) => {
                debug!("job '{}' completed exit_code: {:?}", job.cmd, exit);
                last_exit_code = i32::from(exit);

                // Execute post-exec hooks
                if let Err(e) = shell.exec_post_exec_hooks(&job.cmd, exit as i32) {
                    debug!("Error executing post-exec hooks: {}", e);
                }
            }
            Err(err) => {
                ctx.pid = None;
                ctx.pgid = None;
                enable_raw_mode().ok(); // Restore raw mode before returning error
                return Err(err);
            }
        }
        // reset
        ctx.pid = None;
        ctx.pgid = None;

        // Re-enable raw mode after each job completes
        enable_raw_mode().ok();

        gate_op = next_gate_op;

        if stop_processing {
            break;
        }
    }

    debug!("EVAL_STR: Job loop completed");
    Ok(last_exit_code)
}

/// Execute a job and capture its stdout and stderr
/// Returns (exit_code, stdout, stderr)
pub async fn execute_with_capture(
    _shell: &mut Shell,
    _ctx: &mut Context,
    job: &Job,
) -> Result<(i32, String, String)> {
    use std::process::Stdio;
    use tokio::process::Command;

    // Strip the |> suffix from the command
    let cmd_str = job.cmd.trim();
    let cmd_str = cmd_str.strip_suffix("|>").unwrap_or(cmd_str).trim();

    debug!("Executing with capture: '{}'", cmd_str);

    // Use sh -c to execute the command via Tokio (async)
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd_str)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
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

pub fn get_jobs(shell: &mut Shell, input: &str) -> Result<Vec<Job>> {
    let (input_cow, pairs_opt) =
        parser::parse_with_expansion(input, Arc::clone(&shell.environment))?;

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

// SAFETY WARNING:
// This function calls `fork()` in a potentially multi-threaded environment (Tokio runtime).
// In the child process (ForkResult::Child), it proceeds to use `job.launch` which is async
// and relies on the Tokio runtime.
//
// Using `fork` without `exec` in a multi-threaded program is generally unsafe because
// only the thread calling fork is duplicated. If other threads held locks (like malloc locks
// or Tokio internal locks), those locks are now held forever in the child, leading to deadlocks.
//
// Ideally, subshells should be implemented by re-executing the shell binary with specific flags,
// or by using a dedicated process spawner that avoids this pattern.
// Proceed with caution.
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
            // Child process
            // SAFETY: Do NOT use tracing here. Unsafe after fork.
            let pid = getpid();
            // setpgid is syscall
            if setpgid(pid, pid).is_err() {
                // ignore or raw write
            }

            job.pgid = Some(pid);
            ctx.pgid = Some(pid);

            // Execute
            let res = job.launch(ctx, shell).await;

            if let Ok(ProcessState::Completed(exit, _)) = res {
                std::process::exit(i32::from(exit));
            } else {
                std::process::exit(-1);
            }
        }
    }
}

fn transform_input_for_smart_pipe(input: String) -> String {
    let trimmed = input.trim_start();
    // Check if it starts with | but not |> (capture) or || (OR operator)
    if trimmed.starts_with('|') && !trimmed.starts_with("|>") && !trimmed.starts_with("||") {
        debug!("Smart Pipe triggered: prepending output history");
        format!("__dsh_print_last_stdout {}", input)
    } else {
        input
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;
    use crate::shell::Shell;

    #[test]
    fn test_get_jobs_simple() {
        let env = Environment::new();
        let mut shell = Shell::new(env);
        let jobs = get_jobs(&mut shell, "echo hello").unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].cmd, "echo hello");
    }

    #[test]
    fn test_get_jobs_sequence() {
        let env = Environment::new();
        let mut shell = Shell::new(env);
        let jobs = get_jobs(&mut shell, "echo a; echo b").unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].cmd, "echo a");
        assert_eq!(jobs[1].cmd, "echo b");
    }

    #[test]
    fn test_get_jobs_background() {
        let env = Environment::new();
        let mut shell = Shell::new(env);
        let jobs = get_jobs(&mut shell, "echo a &").unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].cmd, "echo a &");
        assert!(!jobs[0].foreground);
    }

    #[test]
    fn test_transform_input_for_smart_pipe() {
        // Normal cases (no change)
        assert_eq!(
            transform_input_for_smart_pipe("ls -la".to_string()),
            "ls -la"
        );
        assert_eq!(
            transform_input_for_smart_pipe("echo hello".to_string()),
            "echo hello"
        );
        assert_eq!(
            transform_input_for_smart_pipe("|| echo fail".to_string()),
            "|| echo fail"
        );
        assert_eq!(
            transform_input_for_smart_pipe("|> out.txt".to_string()),
            "|> out.txt"
        );

        // Smart pipe cases
        assert_eq!(
            transform_input_for_smart_pipe("| grep foo".to_string()),
            "__dsh_print_last_stdout | grep foo"
        );
        assert_eq!(
            transform_input_for_smart_pipe("  | grep foo".to_string()),
            "__dsh_print_last_stdout   | grep foo"
        );
    }

    #[test]
    fn test_transform_smart_pipe_edge_cases() {
        // Capture mode should NOT trigger smart pipe
        assert_eq!(
            transform_input_for_smart_pipe("|> output.txt".to_string()),
            "|> output.txt"
        );

        // Capture mode with command should not change
        assert_eq!(
            transform_input_for_smart_pipe("ls -la |>".to_string()),
            "ls -la |>"
        );

        // OR operator should NOT trigger smart pipe
        assert_eq!(
            transform_input_for_smart_pipe("|| true".to_string()),
            "|| true"
        );

        // Multiple pipes with leading pipe should transform
        assert_eq!(
            transform_input_for_smart_pipe("| head -10 | tail -5".to_string()),
            "__dsh_print_last_stdout | head -10 | tail -5"
        );

        // Just pipe character alone should transform
        assert_eq!(
            transform_input_for_smart_pipe("| wc -l".to_string()),
            "__dsh_print_last_stdout | wc -l"
        );

        // Pipe with various whitespace
        assert_eq!(
            transform_input_for_smart_pipe("\t| sed 's/a/b/g'".to_string()),
            "__dsh_print_last_stdout \t| sed 's/a/b/g'"
        );
    }
}

