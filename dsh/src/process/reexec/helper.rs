//! What runs inside a re-exec helper process.
//!
//! Entered through [`run_internal_helper`] before any interactive startup
//! (no `config.lisp`, no history, no MCP): the helper is an execution detail
//! of the parent session, not a new interactive agent.

use super::fd_layout::{InternalHelperFds, setup_helper_status_fd};
use super::protocol::{
    BuiltinExecRequest, InternalExecKind, NoCommandExecRequest, PipelineSourceExecRequest,
    PlanExecMode, PlanExecRequest, read_internal_request,
};
use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;
use std::os::unix::io::RawFd;

/// Run one helper request to completion inside the fresh process.
///
/// Called before any interactive startup (no `config.lisp`, no history, no
/// MCP, no notebook, no lifecycle activation): the helper is an execution
/// detail of the parent session, not a new interactive agent. Returns the
/// process exit code.
pub async fn run_internal_helper(fds: InternalHelperFds) -> std::process::ExitCode {
    match run_internal_helper_inner(fds).await {
        Ok(code) => std::process::ExitCode::from(code),
        Err(err) => {
            eprintln!("dogesh: internal exec failed: {err:#}");
            std::process::ExitCode::from(1)
        }
    }
}

async fn run_internal_helper_inner(fds: InternalHelperFds) -> Result<u8> {
    fds.validate()?;
    let request = read_internal_request(fds.request)?;
    // The status fd arrives non-CLOEXEC (`dup2` clears the flag on its
    // target): mark it close-on-exec here so nested external children and
    // sub-helpers cannot hold it open past their own `execve`, which would
    // hide the parent's EOF. `None` performs no syscall at all, so a
    // status-less helper (background builtin) can never flip `CLOEXEC` on an
    // unrelated inherited descriptor.
    setup_helper_status_fd(fds.status)?;

    let env_arc = crate::environment::Environment::new();
    {
        let mut env = env_arc.write();
        request.snapshot.apply_to(&mut env);
    }
    // Follow the snapshot cwd explicitly, not spawn inheritance alone.
    if let Err(err) = std::env::set_current_dir(&request.snapshot.cwd) {
        eprintln!(
            "dogesh: internal exec: cannot chdir to {}: {err}",
            request.snapshot.cwd.display()
        );
        return Ok(1);
    }
    let mut shell = Shell::new(env_arc);

    match &request.kind {
        InternalExecKind::Builtin(builtin) => run_helper_builtin(&mut shell, builtin).await,
        InternalExecKind::Plan(plan_request) => {
            run_helper_plan(&mut shell, plan_request, fds.status).await
        }
        InternalExecKind::PipelineSource(source) => run_helper_pipeline_source(source).await,
        InternalExecKind::NoCommand(no_command) => {
            run_helper_no_command(&mut shell, no_command).await
        }
    }
}

async fn run_helper_builtin(shell: &mut Shell, builtin: &BuiltinExecRequest) -> Result<u8> {
    // Command-scoped `NAME=value` prefixes: apply as exported vars in the
    // helper only; the helper exits right after, so nothing leaks back.
    for (key, value) in &builtin.env_overrides {
        shell
            .environment
            .write()
            .set_shell_var(key.clone(), value.clone());
        shell
            .environment
            .write()
            .variable_state
            .exported_vars
            .insert(key.clone());
    }

    let Some(handler) = dsh_builtin::get_handler(&builtin.name) else {
        eprintln!("dogesh: internal exec: unknown builtin: {}", builtin.name);
        return Ok(2);
    };
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
    ctx.foreground = false;
    ctx.interactive = false;
    ctx.save_history = false;
    // Fresh Tokio runtime via the caller's `block_on`: async handlers run
    // their real async implementation, not the sync fallback.
    let status = handler.execute(&ctx, builtin.argv.clone(), shell).await;
    Ok(exit_code_of(status))
}

/// Run one materialized no-command pipeline stage in the helper's fresh
/// shell snapshot: apply assignments, report the last command-substitution
/// status (or 0 when there was none).
///
/// Redirections cross as an empty list on purpose: the parent pipeline
/// launch path already applied them exactly once before spawning, so the
/// helper's stdio is already final. Reopening targets here would run the
/// side effect twice.
async fn run_helper_no_command(shell: &mut Shell, no_command: &NoCommandExecRequest) -> Result<u8> {
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
    ctx.foreground = false;
    ctx.interactive = false;
    ctx.save_history = false;
    let result = crate::shell::no_command::execute_no_command(
        shell,
        &mut ctx,
        crate::shell::no_command::NoCommandMaterialization {
            assignments: no_command.assignments.clone(),
            redirects: Vec::new(),
            last_command_substitution_status: no_command.last_command_substitution_status,
            resources: crate::shell::substitution::ExecutionResources::new(),
        },
    );
    match result {
        crate::shell::no_command::NoCommandExecutionResult::Completed(code) => {
            Ok(code.clamp(0, 255) as u8)
        }
        crate::shell::no_command::NoCommandExecutionResult::Failed(failure) => {
            // Unreachable with an empty redirect list, but fail closed by
            // type rather than by assumption.
            eprintln!(
                "dogesh: internal exec no-command failed: {}",
                failure.message
            );
            Ok(failure.exit_code.clamp(0, 255) as u8)
        }
    }
}

/// Write synthetic source bytes to fd 1 (the pipeline write end wired by
/// `posix_spawn` file actions). A consumer that exited early surfaces as
/// `SIGPIPE`/`EPIPE`: normal upstream termination, never a user-visible
/// error. The pipeline's final status comes from its last downstream stage.
async fn run_helper_pipeline_source(source: &PipelineSourceExecRequest) -> Result<u8> {
    run_helper_pipeline_source_to_fd(source, libc::STDOUT_FILENO).await
}

async fn run_helper_pipeline_source_to_fd(
    source: &PipelineSourceExecRequest,
    fd: RawFd,
) -> Result<u8> {
    let bytes = source.data.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        let ret = unsafe {
            libc::write(
                fd,
                bytes[offset..].as_ptr() as *const libc::c_void,
                bytes.len() - offset,
            )
        };
        if ret < 0 {
            let errno = std::io::Error::last_os_error();
            match errno.raw_os_error() {
                // Interrupted: retry. Closed consumer: quiet normal exit.
                Some(code) if code == libc::EINTR => continue,
                Some(code) if code == libc::EPIPE => return Ok(0),
                _ => {
                    if errno.kind() == std::io::ErrorKind::BrokenPipe {
                        return Ok(0);
                    }
                    anyhow::bail!("pipeline source write failed: {errno}");
                }
            }
        }
        if ret == 0 {
            // Blocking pipe write with remaining bytes must not return 0;
            // treating it as success would silently truncate the source.
            anyhow::bail!("pipeline source write returned zero");
        }
        offset += ret as usize;
    }
    Ok(0)
}

async fn run_helper_plan(
    shell: &mut Shell,
    plan_request: &PlanExecRequest,
    status_fd: Option<RawFd>,
) -> Result<u8> {
    // The helper owns no terminal UI: confirmation prompts must never touch
    // stdout (it is data in capture mode). `helper_confirm` asks on
    // `/dev/tty` and fails closed when there is none.
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
    ctx.foreground = false;
    ctx.interactive = false;
    ctx.save_history = false;
    // The helper's stdout is already the final destination (terminal, or the
    // parent's capture pipe). Marking it as caller-captured suppresses the
    // non-interactive auto-capture: without this every nested external would
    // grow a capture pipe plus an `OutputMonitor` that re-emits its output
    // with a `\r\n` display prefix — byte pollution inside `$(...)`.
    // This mirrors the old in-process substitution loop, which set
    // `captured_out` for the same reason.
    ctx.captured_out = Some(libc::STDOUT_FILENO);
    // Process-substitution producers lead their own process group (spawned
    // with a fresh pgid): preset it so every nested spawn joins the group
    // and the parent's group-kill reaper reaches the whole tree, including
    // grandchildren that outlive the helper itself. Async AND-OR lists
    // work the same way: the helper founds (or joins) the async group and
    // everything it starts must stay inside it.
    if matches!(
        plan_request.mode,
        PlanExecMode::ProcessSubstitution | PlanExecMode::AsyncAndOrList
    ) {
        ctx.pgid = Some(shell.pid);
    }
    // Async-list helpers evaluate under the background policy: a selected
    // session-bound builtin fails closed instead of reading the helper's
    // empty session. All other isolated bodies keep historical behavior.
    let env = match plan_request.mode {
        PlanExecMode::AsyncAndOrList => crate::shell::eval::PlanEvaluationEnvironment::AsyncList,
        PlanExecMode::Subshell
        | PlanExecMode::CommandSubstitution
        | PlanExecMode::ProcessSubstitution => {
            crate::shell::eval::PlanEvaluationEnvironment::Isolated
        }
    };
    let outcome = crate::shell::eval::evaluate_plan_in(
        shell,
        &mut ctx,
        &plan_request.plan,
        helper_confirm,
        env,
    )
    .await;
    match outcome {
        Ok(code) => {
            report_status_byte(status_fd, b'A');
            Ok(code.clamp(0, 255) as u8)
        }
        Err(err) if crate::shell::authorize::is_authorization_cancelled(&err) => {
            // A nested denial must abort the whole outer chain, not surface
            // as empty output: report it on the status fd so the parent maps
            // it back to `AuthorizationCancelled`. Exit 130 either way.
            report_status_byte(status_fd, b'D');
            Ok(130)
        }
        Err(err) => {
            eprintln!("dogesh: internal exec plan failed: {err:#}");
            report_status_byte(status_fd, b'E');
            Ok(1)
        }
    }
}

/// Best-effort one-byte completion report. Failures (parent already gone)
/// are ignored: the exit code still carries the primary signal. `None` —
/// helpers without a status channel — is a no-op that writes nowhere.
fn report_status_byte(status_fd: Option<RawFd>, byte: u8) {
    let Some(fd) = status_fd else {
        return;
    };
    let buf = [byte];
    unsafe {
        libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len());
    }
}

/// Confirmation prompter for re-exec helpers.
///
/// The interactive `confirm_action` draws on stdout with keypress input —
/// unusable here because stdout is captured data and the helper shares the
/// terminal with a blocked parent. This asks one line on `/dev/tty`
/// instead, and answers `No` (fail closed) when no terminal is available, so
/// `$(rm -rf ...)` in a pipeline can never self-approve.
///
/// An `AlwaysAllow` answer only touches the helper's own allowlist, which
/// dies with it; nothing is persisted into the parent session.
fn helper_confirm(message: &str) -> Result<crate::repl::confirmation::ConfirmationAction> {
    use crate::repl::confirmation::ConfirmationAction;
    use std::io::{BufRead, Write};

    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty");
    let Ok(mut tty) = tty else {
        return Ok(ConfirmationAction::No);
    };
    if writeln!(
        tty,
        "SAFETY GUARD (background task): {message}\r\nProceed? [y/N/a(Always for this task)]: "
    )
    .is_err()
    {
        return Ok(ConfirmationAction::No);
    }
    let mut line = String::new();
    let mut reader = std::io::BufReader::new(tty.try_clone().map_err(|err| anyhow::anyhow!(err))?);
    if reader.read_line(&mut line).is_err() {
        return Ok(ConfirmationAction::No);
    }
    Ok(match line.trim().to_lowercase().as_str() {
        "y" | "yes" => ConfirmationAction::Yes,
        "a" | "always" => ConfirmationAction::AlwaysAllow,
        _ => ConfirmationAction::No,
    })
}
fn exit_code_of(status: dsh_types::ExitStatus) -> u8 {
    use dsh_types::ExitStatus;
    match status {
        ExitStatus::ExitedWith(code) if code >= 0 => code.clamp(0, 255) as u8,
        ExitStatus::ExitedWith(_) => 1,
        // A detached pid or control-flow marker inside a one-shot helper has
        // nothing left to report to: success.
        ExitStatus::Running(_) | ExitStatus::Break | ExitStatus::Continue | ExitStatus::Return => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{builtin_request, pipe_with_request};
    use super::*;

    #[tokio::test]
    async fn helper_runs_builtin_and_reports_status() {
        // `cd` to a missing directory: the async handler runs for real in
        // the helper and its non-zero status becomes the exit code.
        let request = builtin_request("cd", vec!["cd".to_string(), "/dsh-no-such-dir".to_string()]);
        let fd = pipe_with_request(&request);
        let fds = InternalHelperFds {
            request: fd,
            status: None,
        };
        let code = run_internal_helper_inner(fds).await.expect("helper runs");
        assert_eq!(code, 1);
    }

    #[tokio::test]
    async fn helper_rejects_unknown_builtin_without_panic() {
        let request = builtin_request(
            "dsh-no-such-builtin",
            vec!["dsh-no-such-builtin".to_string()],
        );
        let fd = pipe_with_request(&request);
        let fds = InternalHelperFds {
            request: fd,
            status: None,
        };
        let code = run_internal_helper_inner(fds).await.expect("helper runs");
        assert_ne!(code, 0);
    }

    #[tokio::test]
    async fn helper_pipeline_source_writes_exact_bytes_to_pipe() {
        use super::super::protocol::PipelineSourceExecRequest;
        use std::os::fd::{FromRawFd, IntoRawFd};

        let data = "hello-smart-pipe\n".to_string();
        let request = PipelineSourceExecRequest { data: data.clone() };
        let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
        let write_fd = write.into_raw_fd();
        let code = run_helper_pipeline_source_to_fd(&request, write_fd)
            .await
            .expect("source writes");
        assert_eq!(code, 0);
        unsafe { libc::close(write_fd) };
        let mut output = Vec::new();
        use std::io::Read as _;
        unsafe { std::fs::File::from_raw_fd(read.into_raw_fd()) }
            .read_to_end(&mut output)
            .expect("read");
        assert_eq!(String::from_utf8_lossy(&output), data);
    }

    #[tokio::test]
    async fn helper_pipeline_source_empty_and_large_roundtrip() {
        use super::super::protocol::PipelineSourceExecRequest;
        use std::os::fd::{FromRawFd, IntoRawFd};

        // Empty fits any buffer; large needs a concurrent drainer so the
        // blocking writer never stalls on a full pipe.
        let cases = [String::new(), "y\n".repeat(70_000)];
        for data in cases {
            let request = PipelineSourceExecRequest { data: data.clone() };
            let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
            let write_fd = write.into_raw_fd();
            let read_fd = read.into_raw_fd();
            let reader = std::thread::spawn(move || {
                let mut output = Vec::new();
                use std::io::Read as _;
                unsafe { std::fs::File::from_raw_fd(read_fd) }
                    .read_to_end(&mut output)
                    .expect("read");
                output
            });
            let code = run_helper_pipeline_source_to_fd(&request, write_fd)
                .await
                .expect("source writes");
            assert_eq!(code, 0);
            unsafe { libc::close(write_fd) };
            let output = reader.join().expect("reader thread");
            assert_eq!(String::from_utf8_lossy(&output), data);
        }
    }

    #[tokio::test]
    async fn helper_no_command_applies_assignments_and_reports_status() {
        use super::super::protocol::NoCommandExecRequest;

        let mut shell = Shell::new(crate::environment::Environment::new());
        // Assignment-only: exit 0, assignments land in the helper shell.
        let code = run_helper_no_command(
            &mut shell,
            &NoCommandExecRequest {
                assignments: vec![("HELPER_NO_CMD".to_string(), "bar".to_string())],
                last_command_substitution_status: None,
            },
        )
        .await
        .expect("helper runs");
        assert_eq!(code, 0);
        assert_eq!(
            shell
                .environment
                .read()
                .lookup_variable("HELPER_NO_CMD")
                .as_deref(),
            Some("bar")
        );
        // The last command-substitution status becomes the exit code.
        let code = run_helper_no_command(
            &mut shell,
            &NoCommandExecRequest {
                assignments: vec![],
                last_command_substitution_status: Some(7),
            },
        )
        .await
        .expect("helper runs");
        assert_eq!(code, 7);
    }

    #[tokio::test]
    async fn helper_rejects_stdio_and_aliased_fds() {
        // A hand-invoked hidden flag must never claim stdio or alias the
        // two channels: clean errors, no panic, no descriptor touched.
        let request = builtin_request("cd", vec!["cd".to_string()]);
        let fd = pipe_with_request(&request);
        let fds = InternalHelperFds {
            request: 1,
            status: None,
        };
        assert!(run_internal_helper_inner(fds).await.is_err());
        let fds = InternalHelperFds {
            request: fd,
            status: Some(fd),
        };
        assert!(run_internal_helper_inner(fds).await.is_err());
        // Validation owns nothing: the pipe read end is still ours to close.
        unsafe { libc::close(fd) };
    }
}
