use crate::process::{Job, ListOp, ProcessState};
use crate::shell::{
    Shell,
    authorize::{AuthorizationDecision, authorize_job, is_authorization_cancelled},
    materialize::{MaterializeOutcome, materialize_job},
    parse::parse_execution_plan,
};
use crate::terminal::title;
use anyhow::Result;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dsh_builtin::execute_chat_message;
use dsh_types::{Context, ExitStatus};
use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
use std::io::Write;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::Arc;
use tracing::debug;

mod plan_eval;
mod subshell;
pub(crate) use plan_eval::evaluate_plan;
pub use subshell::execute_with_capture;

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
        let lifecycle = crate::agent_lifecycle::current(shell);
        let _turn = lifecycle.begin_turn();
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

    // Smart Pipe: a line-head `|` reuses previous output as a synthetic
    // pipeline source. The downstream text alone goes to the parser; the
    // first job is marked with `PreviousOutput` and keeps the original
    // `| ...` line as its user-facing source.
    let plan = match parse_plan_with_smart_pipe(&input, Arc::clone(&shell.environment)) {
        Ok(plan) => plan,
        Err(err) => {
            publish_exit_status(shell, 1);
            return Err(err);
        }
    };

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
    for planned in &plan.jobs {
        // `list_op` is stored on the *previous* job by the parser.
        // We keep it here before moving `job` into wait_jobs.
        let next_gate_op = planned.list_op.clone();

        ctx.infile = base_infile;
        ctx.outfile = base_outfile;
        ctx.errfile = base_errfile;

        // Gating comes before materialization: a skipped branch performs no
        // substitution, no pipe, and no authorization prompt.
        let should_run = match gate_op {
            ListOp::None => true,
            ListOp::And => last_exit_code == 0,
            ListOp::Or => last_exit_code != 0,
        };

        if !should_run {
            debug!(
                "skip job '{}' due to gate_op:{:?} last_exit_code:{}",
                planned.source, gate_op, last_exit_code
            );
            gate_op = next_gate_op;
            continue;
        }

        // Materialize only the selected job. Nested substitution bodies were
        // authorized inside this call; a nested denial aborts the whole line.
        // A rejected builtin prefix is an ordinary command failure: publish
        // its status and continue the list so `&&`/`||` gate correctly.
        // Diagnostic goes through `ctx` (not a hard-coded process stderr) so
        // capture/helper/test stdio stays coherent. Redirects are not yet
        // applied here, so `2>` on the same line does not catch this message;
        // status/gating correctness is what this path guarantees.
        let materialized = match materialize_job(
            shell,
            ctx,
            planned,
            crate::repl::confirmation::confirm_action,
        )
        .await
        {
            Ok(MaterializeOutcome::Runnable(materialized)) => materialized,
            Ok(MaterializeOutcome::NoCommand(no_command)) => {
                // Expansion left no command name: assignments, redirections,
                // and the last substitution status still run through the
                // shared no-command executor, exactly as in helpers.
                match crate::shell::no_command::execute_no_command(shell, ctx, *no_command) {
                    crate::shell::no_command::NoCommandExecutionResult::Completed(code) => {
                        last_exit_code = code;
                    }
                    crate::shell::no_command::NoCommandExecutionResult::Failed {
                        exit_code,
                        message,
                    } => {
                        let _ = ctx.write_stderr(&message);
                        last_exit_code = exit_code;
                    }
                }
                publish_exit_status(shell, last_exit_code);
                gate_op = next_gate_op;
                continue;
            }
            Ok(MaterializeOutcome::Rejected(failure)) => {
                let _ = ctx.write_stderr(&failure.message);
                last_exit_code = failure.exit_code;
                publish_exit_status(shell, last_exit_code);
                gate_op = next_gate_op;
                continue;
            }
            Err(err) if is_authorization_cancelled(&err) => {
                tracing::info!("Command execution cancelled by user (nested)");
                publish_exit_status(shell, 130);
                return Ok(130);
            }
            Err(err) => return Err(err),
        };
        let mut job = materialized.job;
        let had_dynamic = materialized.had_dynamic_expansion;
        job.resources = materialized.resources;
        match authorize_job(shell, &job, had_dynamic)? {
            AuthorizationDecision::Allow => {}
            AuthorizationDecision::Deny => {
                tracing::info!("Command execution cancelled by user");
                publish_exit_status(shell, 130);
                return Ok(130);
            }
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
        // Hand this pane's Herdr lifecycle authority to a recognized agent
        // CLI (`codex`, `claude`, ...) for as long as it runs in the
        // foreground, so Herdr's own detection can classify the pane
        // instead of leaving it stuck on whatever this shell last reported.
        // A complete no-op when Herdr isn't active or `job` isn't such a
        // command. See `agent_lifecycle::yield_to_foreground_agent`.
        let _agent_handoff = crate::agent_lifecycle::yield_to_foreground_agent(
            shell,
            &job,
            ctx.interactive,
            job.foreground,
        );

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
            publish_exit_status(shell, last_exit_code);
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
                publish_exit_status(shell, last_exit_code);
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
            publish_exit_status(shell, last_exit_code);
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
            Ok(state @ ProcessState::Completed(_, _)) => {
                let exit = state
                    .shell_exit_code()
                    .expect("completed state has exit code");
                debug!("job '{}' completed exit_code: {:?}", job.cmd, exit);
                last_exit_code = exit;

                // Execute post-exec hooks
                if let Err(e) = shell.exec_post_exec_hooks(&job.cmd, exit) {
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

        publish_exit_status(shell, last_exit_code);
        gate_op = next_gate_op;

        if stop_processing {
            break;
        }
    }

    debug!("EVAL_STR: Job loop completed");
    publish_exit_status(shell, last_exit_code);

    Ok(last_exit_code)
}

/// Record the status `$?` should report.
///
/// Every exit from `eval_str` goes through here, refusals and cancellations
/// included: a line that was blocked still happened, and leaving the previous
/// line's status in place would tell the user it succeeded.
///
/// `publish_exit_status` updates after each job completes, so later jobs on
/// the same line resolve `$?` during their own materialization.
pub(crate) fn publish_exit_status(shell: &Shell, code: i32) {
    shell.environment.write().last_exit_status = code;
}

/// Static job projection for safety checks and tests: pure planning plus
/// static materialization. Never executes substitutions and never mutates
/// shell state (standalone assignments become "no job", as before).
///
/// Fail closed: malformed input is a syntax error here, so callers never
/// judge a parsed prefix while the whole line runs.
pub fn get_jobs(shell: &mut Shell, input: &str) -> Result<Vec<Job>> {
    let plan = parse_plan_with_smart_pipe(input, Arc::clone(&shell.environment))?;
    crate::shell::materialize::dry_materialize_plan(&plan, shell)
}

/// Whether a line is a Smart Pipe continuation: line-head `|` that is not
/// the `|>` capture syntax and not the `||` OR operator. Leading whitespace
/// is allowed.
pub(crate) fn is_smart_pipe_input(input: &str) -> bool {
    let trimmed = input.trim_start();
    trimmed.starts_with('|') && !trimmed.starts_with("|>") && !trimmed.starts_with("||")
}

/// Parse with Smart Pipe support: a line-head `|` parses only its
/// downstream text, marks the first job with `PreviousOutput`, and keeps the
/// original `| ...` line as the user-facing source.
pub(crate) fn parse_plan_with_smart_pipe(
    input: &str,
    environment: Arc<parking_lot::RwLock<crate::environment::Environment>>,
) -> Result<crate::shell::plan::ExecutionPlan> {
    use crate::shell::plan::PlannedPipelineSource;
    if !is_smart_pipe_input(input) {
        return parse_execution_plan(input, environment);
    }
    let trimmed = input.trim_start();
    // SAFETY: `is_smart_pipe_input` verified the leading `|`.
    let downstream = trimmed[1..].trim_start();
    if downstream.is_empty() {
        // A bare `|` is a no-op success, consistent with an empty line:
        // there is no downstream command to materialize. This differs from
        // `| FOO=bar`, which has a downstream stage that expands to no
        // command and is rejected as a pipeline failure.
        return Ok(crate::shell::plan::ExecutionPlan::default());
    }
    let mut plan = parse_execution_plan(downstream, environment)?;
    if let Some(first) = plan.jobs.first_mut() {
        first.pipeline_source = Some(PlannedPipelineSource::PreviousOutput);
        // User-facing source keeps the line-head pipe, but only for this
        // job: with `| grep foo; echo hi` the second job must stay `echo hi`,
        // not inherit the whole line (which would pollute history, safety
        // messages, and exact-source allowlist matching).
        first.source = format!("| {}", first.source.trim_start());
    }
    debug!(
        "Smart Pipe: downstream={downstream:?} jobs={}",
        plan.jobs.len()
    );
    Ok(plan)
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
    fn test_smart_pipe_detection() {
        assert!(!is_smart_pipe_input("ls -la"));
        assert!(!is_smart_pipe_input("echo hello"));
        assert!(!is_smart_pipe_input("|| echo fail"));
        assert!(!is_smart_pipe_input("|> out.txt"));
        assert!(!is_smart_pipe_input("ls -la |>"));
        assert!(!is_smart_pipe_input("|| true"));
        assert!(is_smart_pipe_input("| grep foo"));
        assert!(is_smart_pipe_input("  | grep foo"));
        assert!(is_smart_pipe_input("| head -10 | tail -5"));
        assert!(is_smart_pipe_input("| wc -l"));
        assert!(is_smart_pipe_input("\t| sed 's/a/b/g'"));
    }

    #[test]
    fn test_smart_pipe_plan_marks_source_and_keeps_original() {
        use crate::shell::plan::PlannedPipelineSource;
        let env = Environment::new();
        let plan = parse_plan_with_smart_pipe("| grep foo", Arc::clone(&env)).expect("plan");
        assert_eq!(plan.jobs.len(), 1);
        assert_eq!(
            plan.jobs[0].pipeline_source,
            Some(PlannedPipelineSource::PreviousOutput)
        );
        assert_eq!(plan.jobs[0].source, "| grep foo");
        assert_eq!(plan.jobs[0].stages.len(), 1);

        let plan = parse_plan_with_smart_pipe("  | grep foo", Arc::clone(&env)).expect("plan");
        assert_eq!(
            plan.jobs[0].pipeline_source,
            Some(PlannedPipelineSource::PreviousOutput)
        );

        let plan =
            parse_plan_with_smart_pipe("| head -10 | tail -5", Arc::clone(&env)).expect("plan");
        assert_eq!(plan.jobs[0].stages.len(), 2);
        assert_eq!(
            plan.jobs[0].pipeline_source,
            Some(PlannedPipelineSource::PreviousOutput)
        );

        // Ordinary lines carry no source marker.
        let plan = parse_plan_with_smart_pipe("echo hello", Arc::clone(&env)).expect("plan");
        assert_eq!(plan.jobs[0].pipeline_source, None);
    }

    #[test]
    fn test_smart_pipe_multi_job_keeps_per_job_source() {
        use crate::shell::plan::PlannedPipelineSource;
        let env = Environment::new();
        let plan =
            parse_plan_with_smart_pipe("| grep foo; echo hi", Arc::clone(&env)).expect("plan");
        assert_eq!(plan.jobs.len(), 2);
        assert_eq!(
            plan.jobs[0].pipeline_source,
            Some(PlannedPipelineSource::PreviousOutput)
        );
        assert_eq!(plan.jobs[0].source, "| grep foo");
        assert_eq!(plan.jobs[1].pipeline_source, None);
        assert_eq!(plan.jobs[1].source, "echo hi");
    }
}
