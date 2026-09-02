use crate::parser::{self, Rule, ShellParser};
use crate::process::{Job, ListOp, ProcessState, wait_pid_job};
use crate::shell::{
    Shell,
    parse::{ParseContext, parse_commands},
};
use crate::terminal::title;
use anyhow::{Context as _, Result, anyhow};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dsh_builtin::execute_chat_message;
use dsh_types::{Context, ExitStatus};
use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
use nix::unistd::{ForkResult, Pid, fork, getpid, setpgid};
use pest::Parser;
use std::io::Write;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::Arc;
use tokio::task;
use tracing::debug;

struct TitleGuard {
    active: bool,
}

impl TitleGuard {
    fn new(ctx: &Context, job: &Job) -> Self {
        let active = ctx.interactive && job.foreground;
        if active {
            title::set_running_title(job).ok();
        }
        Self { active }
    }
}

impl Drop for TitleGuard {
    fn drop(&mut self) {
        if self.active {
            title::reset_title().ok();
        }
    }
}

pub async fn eval_str(
    shell: &mut Shell,
    ctx: &mut Context,
    input: String,
    force_background: bool,
) -> Result<i32> {
    // Every `enable_raw_mode()` below is gated on `ctx.interactive`, but the
    // matching `disable_raw_mode()` is not, and nothing puts raw mode back on
    // the error paths. Anchor the whole function to the state it was entered
    // with so a caller that was not in raw mode (a test, a `-c` run) cannot
    // exit with the terminal left raw.
    let _raw_mode = crate::repl::terminal_state::RawModeRestore::new();

    if ctx.save_history
        && let Some(ref mut history) = shell.cmd_history
    {
        // Apply secret filtering before saving to history
        let filtered_cmd = shell
            .environment
            .read()
            .policy_state
            .secret_manager
            .process_for_history(&input);

        if let Some(cmd_to_save) = filtered_cmd {
            let mut history = history.lock();
            if let Err(e) = history.write_history(&cmd_to_save) {
                debug!("Failed to write history: {}", e);
            }
        } else {
            debug!("Command skipped from history due to secret detection");
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
        if let Ok(mut termios) =
            tcgetattr(unsafe { BorrowedFd::borrow_raw(std::io::stdin().as_raw_fd()) })
            && !termios.local_flags.contains(LocalFlags::ISIG)
        {
            termios.local_flags.insert(LocalFlags::ISIG);
            if let Err(e) = tcsetattr(
                unsafe { BorrowedFd::borrow_raw(std::io::stdin().as_raw_fd()) },
                SetArg::TCSANOW,
                &termios,
            ) {
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
        // Only re-enable raw mode in interactive context
        if ctx.interactive {
            enable_raw_mode().ok();
        }
        return Ok(code);
    }

    // Smart Pipe transformation
    let input = transform_input_for_smart_pipe(input);

    let jobs = get_jobs(shell, &input)?;

    // SAFETY CHECK
    {
        use crate::repl::confirmation::ConfirmationAction;
        use crate::safety::SafetyResult;

        let _allowlist_add_cmd: Option<String> = None;
        let mut user_cancelled = false;

        {
            let environment = shell.environment.read();
            let safety_level_guard = environment.policy_state.safety_level.read();
            // What the user types is judged against the configured list *and*
            // whatever they waved through earlier in this session. The agent
            // sees only the first of the two.
            let allowlist_guard = environment.policy_state.execute_allowlist.read();
            let always_guard = environment.policy_state.shell_always_allowlist.read();
            let allowlist: Vec<String> = allowlist_guard
                .iter()
                .chain(always_guard.iter())
                .cloned()
                .collect();

            match shell
                .safety_guard
                .check_jobs(&jobs, &safety_level_guard, &allowlist)
            {
                SafetyResult::Allowed => {
                    // Proceed
                }
                SafetyResult::Confirm(reason) => {
                    // Release locks before confirmation to avoid holding them during user input
                    drop(allowlist_guard);
                    drop(always_guard);
                    drop(safety_level_guard);
                    drop(environment);

                    match crate::repl::confirmation::confirm_action(&reason) {
                        Ok(ConfirmationAction::Yes) => {
                            // Proceed
                        }
                        Ok(ConfirmationAction::AlwaysAllow) => {
                            // Proceed and mark for add.
                            // We allow the *exact matching command strings* of all jobs in this approved pipeline.
                            //
                            // This goes to the shell's own store, not the list
                            // the chat agent reads: approving a command for
                            // yourself is not approving it for the AI.
                            shell
                                .environment
                                .read()
                                .policy_state
                                .shell_always_allowlist
                                .write()
                                .extend(jobs.iter().map(|j| j.cmd.clone()));
                        }
                        Ok(ConfirmationAction::No) | Err(_) => {
                            user_cancelled = true;
                        }
                    }
                }
            }
        }

        if user_cancelled {
            tracing::info!("Command execution cancelled by user");
            publish_exit_status(shell, 130);
            return Ok(130);
        }
    }

    let mut last_exit_code = 0_i32;
    // Operator that gates execution of the *current* job based on the previous job result.
    // This is effectively "the separator between previous and current job".
    let mut gate_op = ListOp::None;
    // Every job in the list starts from the stdio the caller handed us. Launching
    // a job rewires `ctx` (pipes, capture, redirections) and nothing put it back,
    // so without this the second job of `a; b` inherits the first one's pipe.
    let base_infile = ctx.infile;
    let base_outfile = ctx.outfile;
    let base_errfile = ctx.errfile;
    for mut job in jobs {
        // `list_op` is stored on the *previous* job by the parser.
        // We keep it here before moving `job` into wait_jobs.
        let next_gate_op = job.list_op.clone();

        ctx.infile = base_infile;
        ctx.outfile = base_outfile;
        ctx.errfile = base_errfile;

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
            job.cmd, job.foreground, job.redirects, job.list_op, job.capture_output,
        );
        let _title_guard = TitleGuard::new(ctx, &job);

        // Handle capture mode with |>
        if job.capture_output {
            let (exit, stdout, stderr) = execute_with_capture(shell, ctx, &mut job).await?;
            last_exit_code = exit;

            // Save to output history
            {
                use dsh_types::output_history::OutputEntry;
                let entry = OutputEntry::new(job.cmd.clone(), stdout.clone(), stderr.clone(), exit);
                shell
                    .environment
                    .write()
                    .session_output_state
                    .output_history
                    .push(entry);
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

            // Execute post-exec hooks
            if let Err(e) = shell.exec_post_exec_hooks(&job.cmd, last_exit_code) {
                debug!("Error executing post-exec hooks: {}", e);
            }

            // Re-enable raw mode after capture job (only in interactive mode)
            if ctx.interactive {
                enable_raw_mode().ok();
            }
            gate_op = next_gate_op;
            continue;
        }

        // Handle struct_pipe mode with |: (Lisp expressions on command output)
        if !job.struct_pipe_exprs.is_empty() {
            use crate::lisp::{Symbol, Value};

            if !job.has_process() {
                debug!("Struct pipe: no executable process, skipping");
                gate_op = next_gate_op;
                continue;
            }

            debug!(
                "Struct pipe: executing command '{}' with {} Lisp expressions",
                job.cmd,
                job.struct_pipe_exprs.len()
            );

            // Declarative output schema for the pipeline's last external
            // command: inject preferred machine-readable flags before the
            // run, parse the captured output into a table after it.
            let schema_spec = job
                .last_external_argv()
                .and_then(|argv| crate::output_schema::lookup(&argv));
            if let Some(prefer) = schema_spec.as_ref().and_then(|spec| spec.prefer.as_ref()) {
                debug!(
                    "Struct pipe: injecting schema args {:?}",
                    prefer.inject_args
                );
                job.append_args_to_last_external(&prefer.inject_args);
            }

            // Execute command through regular job launch path and capture output.
            let (exit_code, output, stderr_output) =
                execute_with_capture(shell, ctx, &mut job).await?;
            last_exit_code = exit_code;

            // Output stderr to terminal (struct_pipe only processes stdout)
            if !stderr_output.is_empty() {
                eprint!("{}", stderr_output);
                std::io::stderr().flush().ok();
            }

            // If command failed and no output, skip Lisp evaluation
            if last_exit_code != 0 && output.is_empty() {
                debug!("Struct pipe: command failed with no output, skipping Lisp eval");
                if ctx.interactive {
                    enable_raw_mode().ok();
                }
                gate_op = next_gate_op;
                continue;
            }

            // With a matching schema and a successful run, hand the Lisp side
            // a typed table in `$_`. Parse failures fall back to the plain
            // string: a schema must never break the pipeline.
            let table = (last_exit_code == 0)
                .then_some(schema_spec.as_ref())
                .flatten()
                .and_then(
                    |spec| match crate::output_schema::parse_with_spec(spec, &output) {
                        Ok(table) => Some(table),
                        Err(err) => {
                            debug!("Struct pipe: schema parse failed, using raw string: {err}");
                            None
                        }
                    },
                );

            // `$RAW` is the raw text of *this* command. The Lisp root
            // environment outlives the pipeline, so it is rebound on every run
            // — leaving a previous command's output in place would silently
            // feed stale data to a later `|:`.
            {
                let engine = shell.lisp_engine.borrow();
                engine
                    .env
                    .borrow_mut()
                    .define(Symbol::from("$RAW"), Value::String(output.clone()));
            }

            // Evaluate Lisp expressions in sequence, passing output through $_
            let mut current_value = match table {
                Some(table) => {
                    Value::Table(crate::lisp::TableRc::new(std::cell::RefCell::new(table)))
                }
                None => Value::String(output),
            };

            for lisp_expr in &job.struct_pipe_exprs {
                debug!("Struct pipe: evaluating Lisp expression: {}", lisp_expr);

                // Bind $_ to current value
                {
                    let engine = shell.lisp_engine.borrow();
                    engine
                        .env
                        .borrow_mut()
                        .define(Symbol::from("$_"), current_value.clone());
                }

                // Evaluate the Lisp expression
                match shell.lisp_engine.borrow().run(lisp_expr) {
                    Ok(result) => {
                        debug!("Struct pipe: Lisp result: {:?}", result);
                        current_value = result;
                    }
                    Err(e) => {
                        eprintln!("Struct pipe error: {}", e);
                        last_exit_code = 1;
                        break;
                    }
                }
            }

            // Print final result (unless it's NIL)
            if current_value != Value::NIL {
                println!("{}", current_value);
            }

            // Execute post-exec hooks
            if let Err(e) = shell.exec_post_exec_hooks(&job.cmd, last_exit_code) {
                debug!("Error executing post-exec hooks: {}", e);
            }

            // Re-enable raw mode after struct_pipe job (only in interactive mode)
            if ctx.interactive {
                enable_raw_mode().ok();
            }
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
                // Restore raw mode only in interactive mode
                if ctx.interactive {
                    enable_raw_mode().ok();
                }
                return Err(err);
            }
        }
        // reset
        ctx.pid = None;
        ctx.pgid = None;

        // Re-enable raw mode after each job completes (only in interactive mode)
        if ctx.interactive {
            enable_raw_mode().ok();
        }

        gate_op = next_gate_op;

        if stop_processing {
            break;
        }
    }

    debug!("EVAL_STR: Job loop completed");
    publish_exit_status(shell, last_exit_code);

    Ok(last_exit_code)
}

/// Execute a job and capture its stdout and stderr
/// Returns (exit_code, stdout, stderr)
pub async fn execute_with_capture(
    shell: &mut Shell,
    ctx: &Context,
    job: &mut Job,
) -> Result<(i32, String, String)> {
    use crate::process::io::cloexec_pipe;
    use libc::STDOUT_FILENO;
    use nix::unistd::close;
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::{FromRawFd, IntoRawFd, RawFd};
    use std::thread;

    fn spawn_pipe_reader(fd: RawFd) -> thread::JoinHandle<std::io::Result<Vec<u8>>> {
        thread::spawn(move || {
            let mut file = unsafe { File::from_raw_fd(fd) };
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            Ok(buf)
        })
    }

    fn join_pipe_reader(
        handle: thread::JoinHandle<std::io::Result<Vec<u8>>>,
        name: &str,
    ) -> Result<String> {
        let bytes = handle
            .join()
            .map_err(|_| anyhow!("{} reader thread panicked", name))?
            .with_context(|| format!("Failed to read {} capture stream", name))?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    let (stdout_read, stdout_write) =
        cloexec_pipe().context("failed to create stdout capture pipe")?;
    let (stderr_read, stderr_write) =
        cloexec_pipe().context("failed to create stderr capture pipe")?;

    let stdout_read_fd = stdout_read.into_raw_fd();
    let stdout_write_fd = stdout_write.into_raw_fd();
    let stderr_read_fd = stderr_read.into_raw_fd();
    let stderr_write_fd = stderr_write.into_raw_fd();

    // Drain capture streams concurrently to avoid pipe-buffer deadlocks on large output.
    let stdout_reader = spawn_pipe_reader(stdout_read_fd);
    let stderr_reader = spawn_pipe_reader(stderr_read_fd);

    let mut capture_ctx = ctx.clone();
    capture_ctx.outfile = STDOUT_FILENO;
    capture_ctx.errfile = stderr_write_fd;
    capture_ctx.captured_out = Some(stdout_write_fd);
    capture_ctx.pid = None;
    capture_ctx.pgid = None;
    capture_ctx.process_count = 0;
    capture_ctx.foreground = true;

    let original_disable_pty = job.disable_pty;
    let original_foreground = job.foreground;
    job.disable_pty = true;
    job.foreground = true;

    let launch_result = job.launch(&mut capture_ctx, shell).await;

    job.disable_pty = original_disable_pty;
    job.foreground = original_foreground;

    // Ensure writer ends are closed in parent so reader threads can finish.
    let _ = close(stdout_write_fd);
    let _ = close(stderr_write_fd);

    let stdout = join_pipe_reader(stdout_reader, "stdout")?;
    let stderr = join_pipe_reader(stderr_reader, "stderr")?;

    let state = launch_result?;
    let exit_code = match state {
        ProcessState::Completed(code, _) => i32::from(code),
        ProcessState::Stopped(_, _) => 130,
        ProcessState::Running => 0,
    };

    debug!(
        "Capture complete: exit={}, stdout={} bytes, stderr={} bytes",
        exit_code,
        stdout.len(),
        stderr.len()
    );

    Ok((exit_code, stdout, stderr))
}

/// Record the status `$?` should report.
///
/// Every exit from `eval_str` goes through here, refusals and cancellations
/// included: a line that was blocked still happened, and leaving the previous
/// line's status in place would tell the user it succeeded.
///
/// Once per line, not once per job: the whole line is parsed and expanded
/// before the first job runs, so a `$?` written on this line was already
/// substituted from the previous one.
fn publish_exit_status(shell: &Shell, code: i32) {
    shell.environment.write().last_exit_status = code;
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
    let Some(pair) = pairs.next() else {
        return Ok(Vec::new());
    };

    // The grammar has no EOI anchor, so `Rule::commands` happily returns a
    // partial match and we would silently execute only the prefix. Report the
    // leftover instead of pretending the whole line ran.
    report_unparsed_tail(&input_cow, pair.as_span().end());

    parse_commands(shell, &mut ctx, pair)
}

/// Warn about input the parser did not consume.
///
/// Kept separate from [`get_jobs`] so the "what counts as leftover" rule is
/// testable without a shell. A trailing separator or whitespace is consumed by
/// the grammar, so anything reaching here is text the user typed and we ignored.
/// What the grammar could not consume, for a caller that must fail closed.
///
/// `get_jobs` only warns about a leftover tail, which is right for a person at
/// the prompt - they can see the warning. It is wrong for a safety check: the
/// verdict would cover the prefix while the whole line runs.
pub fn unconsumed_tail(shell: &mut Shell, input: &str) -> Option<String> {
    let (input_cow, pairs_opt) =
        parser::parse_with_expansion(input, Arc::clone(&shell.environment)).ok()?;

    let mut pairs = match pairs_opt {
        Some(pairs) => pairs,
        None => ShellParser::parse(Rule::commands, &input_cow).ok()?,
    };

    let pair = pairs.next()?;
    parser::unparsed_tail(&input_cow, pair.as_span().end()).map(str::to_string)
}

fn report_unparsed_tail(input: &str, consumed: usize) {
    if let Some(tail) = parser::unparsed_tail(input, consumed) {
        tracing::warn!("unparsed input tail: {:?}", tail);
        // `\r\n`, not `\n`: this runs before raw mode is turned off, where a
        // bare newline leaves the cursor in the same column and staircases the
        // next line.
        eprint!("dsh: warning: ignored unparsed input: {tail}\r\n");
    }
}

pub fn launch_subshell(shell: &mut Shell, ctx: &mut Context, jobs: Vec<Job>) -> Result<()> {
    for mut job in jobs {
        disable_raw_mode().ok();
        let pid = task::block_in_place(|| {
            // Avoid nested-runtime panic by driving only this future directly.
            futures::executor::block_on(spawn_subshell(shell, ctx, &mut job))
        })?;
        debug!("spawned subshell cmd:{} pid: {:?}", job.cmd, pid);
        let res = wait_pid_job(pid, false);
        debug!("wait subshell exit:{:?}", res);
        enable_raw_mode().ok();
    }

    Ok(())
}

/// Run `jobs` in this process and return everything they wrote to stdout.
///
/// Command substitution used to go through `launch_subshell`, which forks. Two
/// things went wrong there: the fork happens under a multi-threaded Tokio
/// runtime, so the child aborted inside Tokio's IO driver as soon as it awaited
/// anything, and the substitution pipe was handed over as a bare `ctx.outfile`,
/// which the non-interactive auto-capture path in `JobProcess::launch` happily
/// overwrote — the result came back empty and the inner output leaked to the
/// terminal.
///
/// Both go away by staying in-process and using the same `captured_out` wiring
/// `execute_with_capture` relies on. The reader runs on its own thread so a
/// result larger than the pipe buffer cannot deadlock the job producing it, and
/// stderr is left alone so diagnostics still reach the terminal.
///
/// What fork used to give away for free — isolation — has to be paid for
/// explicitly. The working directory and the shell variables are snapshotted
/// and restored around the run. Not restored, because a subshell in one process
/// cannot have them: the job table, the Lisp environment, and a bare
/// `NAME=value` inside the substitution, which `parse_command` applies while it
/// is still *parsing* the outer line and so lands before this function is even
/// called.
pub fn capture_subshell_stdout(shell: &mut Shell, ctx: &Context, jobs: Vec<Job>) -> Result<String> {
    use crate::process::io::cloexec_pipe;
    use libc::STDOUT_FILENO;
    // `changepwd` is the single funnel every navigation goes through (cd, z,
    // bookmark, pushd, popd), so restoring through it keeps `OLDPWD`, the
    // directory stack and the chpwd hooks consistent.
    use dsh_builtin::shell_capabilities::ShellNavigation;
    use nix::unistd::close;
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::{FromRawFd, IntoRawFd};

    let (read_end, write_end) = cloexec_pipe().context("failed to create substitution pipe")?;
    let read_fd = read_end.into_raw_fd();
    let write_fd = write_end.into_raw_fd();

    let reader = std::thread::spawn(move || {
        let mut file = unsafe { File::from_raw_fd(read_fd) };
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).map(|_| buf)
    });

    // The jobs run with their own stdio, so hand the terminal back for the
    // duration and restore whatever mode the REPL had set.
    let was_raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
    if was_raw {
        disable_raw_mode().ok();
    }

    // Running in-process means a builtin inside the substitution writes to the
    // *shell's* state: `$(cd /tmp)` moved the whole session and `$(X=1)` left
    // the variable behind. Snapshot what a subshell is supposed to keep to
    // itself and put it back afterwards.
    let entry_dir = std::env::current_dir().ok();
    let entry_vars = {
        let environment = shell.environment.read();
        (
            environment.variable_state.variables.clone(),
            environment.variable_state.exported_vars.clone(),
        )
    };

    let mut launch_result = Ok(());
    let mut last_exit_code = 0_i32;
    // `list_op` lives on the *previous* job, so it gates the one after it.
    let mut gate_op = ListOp::None;
    for mut job in jobs {
        let next_gate_op = job.list_op.clone();
        let should_run = match gate_op {
            ListOp::None => true,
            ListOp::And => last_exit_code == 0,
            ListOp::Or => last_exit_code != 0,
        };
        gate_op = next_gate_op;
        if !should_run {
            continue;
        }

        let mut job_ctx = ctx.clone();
        job_ctx.outfile = STDOUT_FILENO;
        job_ctx.captured_out = Some(write_fd);
        job_ctx.foreground = true;
        job_ctx.pid = None;
        job_ctx.pgid = None;
        job_ctx.process_count = 0;
        job.disable_pty = true;
        job.foreground = true;

        launch_result = task::block_in_place(|| {
            // Avoid nested-runtime panic by driving only this future directly.
            futures::executor::block_on(job.launch(&mut job_ctx, shell))
        })
        .map(|state| {
            debug!("subshell job '{}' finished: {:?}", job.cmd, state);
            if let ProcessState::Completed(code, _) = state {
                last_exit_code = i32::from(code);
            }
        });

        if launch_result.is_err() {
            break;
        }
    }

    // Restore the directory first, so the `OLDPWD` that `changepwd` writes is
    // itself overwritten by the snapshot below.
    if let Some(entry_dir) = entry_dir
        && std::env::current_dir().is_ok_and(|current| current != entry_dir)
        && let Err(err) = shell.changepwd(&entry_dir.to_string_lossy())
    {
        debug!("failed to restore directory after subshell: {}", err);
    }
    {
        let mut environment = shell.environment.write();
        environment.variable_state.variables = entry_vars.0;
        environment.variable_state.exported_vars = entry_vars.1;
        // Putting the maps back by hand skips the setters, so anything derived
        // from a variable the substitution touched has to be rebuilt.
        environment.refresh_derived_state("PATH");
        environment.refresh_derived_state("Z_EXCLUDE");
    }

    if was_raw {
        enable_raw_mode().ok();
    }

    // Close the write end here or the reader never sees EOF.
    let _ = close(write_fd);

    let bytes = reader
        .join()
        .map_err(|_| anyhow!("command substitution reader thread panicked"))?
        .context("failed to read command substitution output")?;

    launch_result?;

    Ok(String::from_utf8_lossy(&bytes).to_string())
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
