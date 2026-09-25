//! The `wait` builtin: wait for known async jobs and report their status.
//!
//! Operands are PIDs or `%`-prefixed job specs (`wait`, `wait PID`,
//! `wait PID1 PID2 ...`, `wait %1`, `wait %+`, `wait %-`, `wait %%`), plus
//! `wait -n` to wait for the next completion among a target set. Bare
//! numbers are always PIDs here, never job numbers: `wait 1` means PID 1,
//! only `wait %1` means job 1. `wait -p`/`-f` stay out of scope and are
//! rejected. Statuses come from `Job::final_exit_status()` using the frozen
//! pipeline policy via the completed-job finalizer; the ledger is consumed
//! only here, never by reconciliation (`jobs`, notices, `fg`/`bg`).
//!
//! `wait -n` never uses `waitpid(-1)`: it polls only the canonical PID set
//! of its resolved targets, so unrelated children (process substitution
//! helpers, detached children, agent children) keep their statuses.

use super::{JobSpec, parse_percent_job_spec, resolve_active_job_spec};
use crate::process::job_wait::{
    JobWaitOutcome, WaitBackoff, check_background_all_output, wait_for_termination,
};
use crate::process::signal::check_and_clear_sigint;
use crate::shell::Shell;
use crate::shell::job::{FinalizeDrain, final_exit_status, finalize_completed_job};
use anyhow::Result;
use dsh_types::Context;
use nix::unistd::Pid;
use std::collections::HashSet;
use tracing::debug;

/// Outcome of waiting for a single PID.
enum WaitOneOutcome {
    /// The PID's status (a waited child, a retained completed status, or
    /// 127 for an unknown PID).
    Status(i32),
    /// `SIGINT` arrived mid-wait: the job was requeued untouched and the
    /// caller must report 130 without touching further operands.
    Interrupted,
}

/// One parsed `wait` operand, before table/ledger resolution.
///
/// A bare decimal is always a PID; only `%`-prefixed forms are job specs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitOperand {
    Pid(Pid),
    Job(JobSpec),
}

/// Why a `wait` operand could not even be parsed (as opposed to a
/// well-formed target no job owns, which reports 127 and continues).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitOperandError {
    Invalid,
}

/// A `wait` operand resolved to the canonical PID the wait layer owns.
///
/// `job_id`/`source` ride along so a future `wait -p` can report *which*
/// job finished without a new lookup path; no `Environment` writes happen
/// here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedWaitTarget {
    pid: Pid,
    job_id: Option<usize>,
    source: WaitTargetSource,
}

/// Where a [`ResolvedWaitTarget`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitTargetSource {
    Pid,
    JobSpec,
}

/// One completed wait-any result: status plus the identity metadata a
/// future `wait -p VAR` needs. Never just a bare status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WaitCompletion {
    pid: Pid,
    job_id: Option<usize>,
    status: i32,
}

/// How a `wait -n` invocation finished.
enum WaitNextOutcome {
    /// Exactly one target completed; only its status was consumed.
    Completed(WaitCompletion),
    /// `SIGINT` arrived mid-wait: every target stays owned, nothing was
    /// consumed, the caller must report 130.
    Interrupted,
    /// No target is (or became) waitable: the caller must report 127.
    NoTargets,
}

/// The parsed `wait` command line: `-n` mode plus raw operand strings.
struct WaitInvocation {
    next: bool,
    operands: Vec<String>,
}

/// `wait` entry point: bridge the async wait onto a runtime shared with
/// `fg`/`jobs`.
pub fn execute_wait(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<i32> {
    super::block_on_job_control_future(wait_async(shell, ctx, argv))?
}

/// Async body of `wait`.
async fn wait_async(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<i32> {
    let invocation = parse_wait_invocation(&argv)?;
    if invocation.next {
        return wait_next_command(shell, ctx, &invocation.operands).await;
    }
    if invocation.operands.is_empty() {
        // Bare `wait`: every known async PID, status always 0 (individual
        // child failures do not become the builtin's status). All entries
        // are consumed.
        for pid in shell.known_async.known_pids() {
            match wait_one(shell, ctx, pid).await? {
                WaitOneOutcome::Status(_) => {}
                WaitOneOutcome::Interrupted => return Ok(130),
            }
        }
        return Ok(0);
    }
    // Sequential `wait`: each operand in order, the last status wins.
    // Unknown targets report 127 without stopping the remaining operands.
    let mut last_status = 0;
    for operand in &invocation.operands {
        match wait_operand(shell, ctx, operand).await? {
            WaitOneOutcome::Status(status) => last_status = status,
            WaitOneOutcome::Interrupted => return Ok(130),
        }
    }
    Ok(last_status)
}

/// Parse the `wait` command line: `-n` mode, `--` end-of-options, operands.
///
/// `-p`/`-f` and any other `-` option stay rejected (usage error, exit 1
/// via the builtin wrapper). A lone `-` is an operand, never an option.
fn parse_wait_invocation(argv: &[String]) -> Result<WaitInvocation> {
    let mut next = false;
    let mut operands = Vec::new();
    let mut end_of_options = false;
    for operand in argv.get(1..).unwrap_or(&[]) {
        if !end_of_options && operand == "--" {
            end_of_options = true;
            continue;
        }
        if !end_of_options
            && operand != "-"
            && let Some(flags) = operand.strip_prefix('-')
        {
            // Bundled short flags (`-nn`) like bash getopt: every flag
            // must be a supported one. Anything else (`-p`, `-f`, `-np`,
            // `-x`) stays a usage error.
            if !flags.is_empty() && flags.chars().all(|flag| flag == 'n') {
                next = true;
                continue;
            }
            // No pre-bail write: the builtin wrapper prints the `Err` once.
            anyhow::bail!("wait: unsupported option: {operand}")
        }
        operands.push(operand.clone());
    }
    Ok(WaitInvocation { next, operands })
}

/// Parse one operand's syntax: `%`-prefixed job spec or decimal PID.
///
/// Bare decimals are always PIDs — even when a job with that number
/// exists. Anything else is [`WaitOperandError::Invalid`].
fn parse_wait_operand(operand: &str) -> Result<WaitOperand, WaitOperandError> {
    if operand.starts_with('%') {
        return parse_percent_job_spec(operand)
            .map(WaitOperand::Job)
            .ok_or(WaitOperandError::Invalid);
    }
    operand
        .parse::<i32>()
        .map(|raw| WaitOperand::Pid(Pid::from_raw(raw)))
        .map_err(|_| WaitOperandError::Invalid)
}

/// Resolve a parsed operand to its canonical PID.
///
/// PID operands resolve unconditionally (unknown-ness is decided by the
/// table/ledger below, which reports 127 without touching `waitpid`).
/// `%+`/`%%`/`%-` resolve against the active table only — a reaped job is
/// not "current". Explicit `%N` falls back to the completed ledger, so a
/// retained status survives `jobs` reconciliation.
fn resolve_wait_operand(shell: &Shell, operand: &WaitOperand) -> Option<ResolvedWaitTarget> {
    match *operand {
        WaitOperand::Pid(pid) => {
            let job_id = shell
                .wait_jobs
                .iter()
                .find(|job| job.pid == Some(pid))
                .map(|job| job.job_id)
                .or_else(|| shell.known_async.job_id_for_pid(pid));
            Some(ResolvedWaitTarget {
                pid,
                job_id,
                source: WaitTargetSource::Pid,
            })
        }
        WaitOperand::Job(spec) => {
            if let Some(index) = resolve_active_job_spec(spec, &shell.wait_jobs) {
                let job = &shell.wait_jobs[index];
                let pid = job.pid.or_else(|| {
                    shell
                        .known_async
                        .entry_by_job_id(job.job_id)
                        .map(|entry| entry.pid)
                })?;
                return Some(ResolvedWaitTarget {
                    pid,
                    job_id: Some(job.job_id),
                    source: WaitTargetSource::JobSpec,
                });
            }
            // Active-table miss: only an explicit `%N` may consult the
            // retained completed ledger.
            if let JobSpec::Number(number) = spec {
                let pid = shell.known_async.pid_by_job_id(number)?;
                return Some(ResolvedWaitTarget {
                    pid,
                    job_id: Some(number),
                    source: WaitTargetSource::JobSpec,
                });
            }
            None
        }
    }
}

/// Wait for one operand: usage errors bail, invalid syntax and unknown
/// targets report 127 and let the remaining operands run.
async fn wait_operand(shell: &mut Shell, ctx: &Context, operand: &str) -> Result<WaitOneOutcome> {
    let parsed = match parse_wait_operand(operand) {
        Ok(parsed) => parsed,
        Err(WaitOperandError::Invalid) => {
            let _ = ctx.write_stderr(&format!("wait: '{operand}': not a pid or job spec"));
            return Ok(WaitOneOutcome::Status(127));
        }
    };
    match resolve_wait_operand(shell, &parsed) {
        Some(target) => wait_target(shell, ctx, target).await,
        None => {
            let _ = ctx.write_stderr(&format!("wait: '{operand}': no such job"));
            Ok(WaitOneOutcome::Status(127))
        }
    }
}

/// Wait for one resolved target: an active table job is taken and
/// termination-waited; an already-completed ledger entry needs no OS wait;
/// anything else is unknown (127) and never touches `waitpid`.
async fn wait_target(
    shell: &mut Shell,
    ctx: &Context,
    target: ResolvedWaitTarget,
) -> Result<WaitOneOutcome> {
    wait_one(shell, ctx, target.pid).await
}

/// Wait for one PID: an active table job is taken and terminated-waited; an
/// already-completed ledger entry needs no OS wait; anything else is
/// unknown (127) and never touches `waitpid`.
async fn wait_one(shell: &mut Shell, ctx: &Context, pid: Pid) -> Result<WaitOneOutcome> {
    if let Some(index) = shell.wait_jobs.iter().position(|job| job.pid == Some(pid)) {
        return wait_active_job(shell, index).await;
    }
    if let Some(exit_status) = shell.known_async.consume_completed(pid) {
        debug!("wait: pid {pid} already completed with status {exit_status}");
        return Ok(WaitOneOutcome::Status(exit_status));
    }
    if shell.known_async.remove(pid).is_some() {
        // Active ledger entry but no table job: stale ownership that can
        // never complete. Drop it rather than blocking forever.
        debug!("wait: pid {pid} has a stale active ledger entry, dropping it");
    }
    let _ = ctx.write_stderr(&format!("wait: '{pid}': not a child of this shell"));
    Ok(WaitOneOutcome::Status(127))
}

/// Termination-wait a table job under temporary ownership (`fg` model).
async fn wait_active_job(shell: &mut Shell, index: usize) -> Result<WaitOneOutcome> {
    let mut job = shell.wait_jobs.remove(index);
    match wait_for_termination(&mut job).await {
        Ok(JobWaitOutcome::Completed) => {
            let job = finalize_completed_job(shell, job, FinalizeDrain::ToEof).await?;
            let status = final_exit_status(&job).ok_or_else(|| {
                anyhow::anyhow!("wait: completed job {} has no final status", job.job_id)
            })?;
            // Archive first, then consume: the status reaches the caller
            // exactly once.
            let raw_pid = job.pid.map(|pid| pid.as_raw()).unwrap_or(-1);
            let consumed = job
                .pid
                .and_then(|pid| shell.known_async.consume_completed(pid));
            debug!(
                "wait: pid {raw_pid} completed with status {status} (ledger consumed: {})",
                consumed.is_some()
            );
            Ok(WaitOneOutcome::Status(status))
        }
        Ok(JobWaitOutcome::Stopped) => {
            // Termination waits never end stopped; reaching here means the
            // policy was bypassed. Requeue rather than orphan.
            debug!(
                "wait: unexpected stop outcome for job {}, requeuing",
                job.job_id
            );
            job.refresh_lifecycle_state();
            shell.wait_jobs.push(job);
            anyhow::bail!("wait: job stopped while waiting for termination")
        }
        Ok(JobWaitOutcome::Interrupted) => {
            job.refresh_lifecycle_state();
            shell.wait_jobs.push(job);
            debug!("wait: interrupted, job requeued as active");
            Ok(WaitOneOutcome::Interrupted)
        }
        Err(err) => {
            if job.is_process_tree_completed() {
                // Infrastructure error after completion: archive first so
                // the status survives, then report the error.
                if let Ok(finalized) =
                    finalize_completed_job(shell, job, FinalizeDrain::ToEof).await
                {
                    let _status = final_exit_status(&finalized);
                }
            } else {
                job.refresh_lifecycle_state();
                shell.wait_jobs.push(job);
            }
            Err(err)
        }
    }
}

/// `wait -n` entry: resolve the target set, then wait for exactly one
/// completion among it.
async fn wait_next_command(shell: &mut Shell, ctx: &Context, operands: &[String]) -> Result<i32> {
    let targets = resolve_wait_next_targets(shell, ctx, operands);
    if targets.is_empty() {
        // No waitable target: unlike bare `wait` (which reports 0 with no
        // known jobs), `wait -n` reports 127. Unknown-operand diagnostics
        // were already printed during resolution.
        return Ok(127);
    }
    match wait_next(shell, targets).await? {
        WaitNextOutcome::Completed(completion) => Ok(completion.status),
        WaitNextOutcome::Interrupted => Ok(130),
        WaitNextOutcome::NoTargets => Ok(127),
    }
}

/// Build the deduplicated `wait -n` target set.
///
/// With operands, only the supplied (valid) targets are watched: an
/// unknown operand is diagnosed but never aborts the valid ones. Without
/// operands, every known async PID is watched in ledger registration
/// order. Process-substitution helpers are never included: only PIDs the
/// shell holds wait ownership of.
fn resolve_wait_next_targets(
    shell: &Shell,
    ctx: &Context,
    operands: &[String],
) -> Vec<ResolvedWaitTarget> {
    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    let mut push_unique = |target: ResolvedWaitTarget| {
        if seen.insert(target.pid.as_raw()) {
            targets.push(target);
        }
    };
    if operands.is_empty() {
        for pid in shell.known_async.known_pids() {
            let job_id = shell
                .wait_jobs
                .iter()
                .find(|job| job.pid == Some(pid))
                .map(|job| job.job_id)
                .or_else(|| shell.known_async.job_id_for_pid(pid));
            push_unique(ResolvedWaitTarget {
                pid,
                job_id,
                source: WaitTargetSource::Pid,
            });
        }
        return targets;
    }
    for operand in operands {
        let parsed = match parse_wait_operand(operand) {
            Ok(parsed) => parsed,
            Err(WaitOperandError::Invalid) => {
                let _ = ctx.write_stderr(&format!("wait: '{operand}': not a pid or job spec"));
                continue;
            }
        };
        // A PID this shell never owned is diagnosed up front (like the
        // sequential path) instead of silently never becoming viable.
        // Stale `Active` entries still prune inside the poll loop, where
        // the remaining targets keep waiting.
        if let WaitOperand::Pid(pid) = parsed
            && !is_wait_known_pid(shell, pid)
        {
            let _ = ctx.write_stderr(&format!("wait: '{pid}': not a child of this shell"));
            continue;
        }
        match resolve_wait_operand(shell, &parsed) {
            Some(target) => push_unique(target),
            None => {
                let _ = ctx.write_stderr(&format!("wait: '{operand}': no such job"));
            }
        }
    }
    targets
}

/// Whether a PID is even worth polling: an active table job or any ledger
/// entry (`Active` or retained `Completed`) for it exists.
fn is_wait_known_pid(shell: &Shell, pid: Pid) -> bool {
    shell.wait_jobs.iter().any(|job| job.pid == Some(pid))
        || shell.known_async.active_entry(pid).is_some()
        || shell.known_async.completed_status(pid).is_some()
}

/// Wait for the next completion among already-resolved targets.
///
/// Never `wait_for_termination` in a loop over targets (that would pin the
/// wait to the first operand). Instead: consume an already-retained
/// completed status first, then nonblocking-poll every active target while
/// draining its output monitors, and finalize exactly the one selected
/// completion through the canonical finalizer. Unselected statuses stay
/// retained for a later `wait`.
async fn wait_next(shell: &mut Shell, targets: Vec<ResolvedWaitTarget>) -> Result<WaitNextOutcome> {
    wait_next_until(shell, targets, check_and_clear_sigint).await
}

/// [`wait_next`] with an injectable interrupt predicate: production passes
/// [`check_and_clear_sigint`], tests pass a plain flag so no real signal
/// delivery (and no cross-test flag theft) is involved.
async fn wait_next_until(
    shell: &mut Shell,
    targets: Vec<ResolvedWaitTarget>,
    mut interrupted: impl FnMut() -> bool,
) -> Result<WaitNextOutcome> {
    // The target set only shrinks through stale-ownership pruning below;
    // deduplication happened at resolution, so no status can be consumed
    // twice for one invocation.
    let mut backoff = WaitBackoff::new();
    loop {
        // A `wait -n` SIGINT interrupts the builtin instead of reaching
        // the background jobs: no forwarding, nothing consumed.
        if interrupted() {
            debug!("wait -n: interrupted, targets stay owned");
            return Ok(WaitNextOutcome::Interrupted);
        }
        // Already-completed fast path, in target order: serve retained
        // statuses without touching the OS wait layer.
        for target in &targets {
            if shell.known_async.completed_status(target.pid).is_some() {
                let status = shell
                    .known_async
                    .consume_completed(target.pid)
                    .expect("completed status peeked above must still be consumable");
                debug!(
                    "wait -n: pid {} already completed with status {status}",
                    target.pid
                );
                return Ok(WaitNextOutcome::Completed(WaitCompletion {
                    pid: target.pid,
                    job_id: target.job_id,
                    status,
                }));
            }
        }
        // Nonblocking poll of every active target. Output monitors must
        // drain here: a target writing past the pipe buffer would otherwise
        // block forever and never terminate. New OS wait decoders are out
        // of scope — `update_status` polls the canonical tree.
        let mut selected: Option<usize> = None;
        for target in &targets {
            if let Some(index) = shell
                .wait_jobs
                .iter()
                .position(|job| job.pid == Some(target.pid))
            {
                check_background_all_output(&mut shell.wait_jobs[index]).await?;
                if shell.wait_jobs[index].update_status() {
                    selected = Some(index);
                    break;
                }
            }
        }
        if let Some(index) = selected {
            return Ok(WaitNextOutcome::Completed(
                finalize_wait_next_selection(shell, index).await?,
            ));
        }
        // Stale pruning: an `Active` ledger entry with no table job can
        // never complete. Drop those, keep waiting on the rest; only when
        // no target is viable at all does `wait -n` give up with 127.
        // `ECHILD` observations never invent a status here: without a
        // completed canonical tree (or a retained ledger status) there is
        // no evidence to report.
        let mut viable = false;
        for target in &targets {
            if shell
                .wait_jobs
                .iter()
                .any(|job| job.pid == Some(target.pid))
            {
                viable = true;
                continue;
            }
            if shell.known_async.completed_status(target.pid).is_some() {
                viable = true;
                continue;
            }
            if shell.known_async.active_entry(target.pid).is_some() {
                debug!(
                    "wait -n: pid {} has a stale active ledger entry, dropping it",
                    target.pid
                );
                shell.known_async.remove(target.pid);
                continue;
            }
            // Unknown target: diagnosed at resolution; it simply never
            // becomes viable.
        }
        if !viable {
            return Ok(WaitNextOutcome::NoTargets);
        }
        tokio::time::sleep(backoff.next_delay()).await;
    }
}

/// Finalize the one `wait -n` selection: canonical finalizer (`ToEof`,
/// like an explicit ownership wait), frozen-policy status, then consume
/// exactly this target's retained status.
async fn finalize_wait_next_selection(shell: &mut Shell, index: usize) -> Result<WaitCompletion> {
    let job = shell.wait_jobs.remove(index);
    let pid = job
        .pid
        .expect("wait -n selection always matched a table PID");
    let job = finalize_completed_job(shell, job, FinalizeDrain::ToEof).await?;
    let status = final_exit_status(&job)
        .ok_or_else(|| anyhow::anyhow!("wait: completed job {} has no final status", job.job_id))?;
    // Archive first, then consume: the status reaches the caller exactly
    // once, and unselected targets keep theirs.
    let consumed = shell.known_async.consume_completed(pid);
    debug!(
        "wait -n: pid {pid} completed with status {status} (ledger consumed: {})",
        consumed.is_some()
    );
    Ok(WaitCompletion {
        pid,
        job_id: Some(job.job_id),
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{JobProcess, Process, ProcessState};

    fn interrupt_flag() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))
    }

    /// `wait -n` under SIGINT reports 130, forwards nothing to the
    /// background job, and keeps every ownership entry intact. The
    /// interrupt arrives as a plain flag (no real signal delivery), so
    /// parallel tests can neither steal nor observe it.
    #[test]
    fn wait_next_interrupt_reports_130_and_keeps_ownership() {
        let flag = interrupt_flag();
        let mut shell = Shell::new(crate::environment::Environment::new());
        let pid = Pid::from_raw(424281);
        let mut job = crate::process::Job::new("sleep 60".to_string(), shell.pgid);
        job.job_id = 1;
        job.pid = Some(pid);
        let mut process = Process::new("sleep".to_string(), vec!["sleep".to_string()]);
        process.pid = Some(pid);
        process.state = ProcessState::Running;
        job.set_process(JobProcess::Command(process));
        shell.wait_jobs.push(job);
        shell.known_async.register(pid, 1);

        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        let probe = flag.clone();
        let outcome = super::super::block_on_job_control_future(wait_next_until(
            &mut shell,
            vec![ResolvedWaitTarget {
                pid,
                job_id: Some(1),
                source: WaitTargetSource::Pid,
            }],
            move || probe.load(std::sync::atomic::Ordering::SeqCst),
        ))
        .expect("bridge executes")
        .expect("wait executes");
        let WaitNextOutcome::Interrupted = outcome else {
            panic!("interrupt must win over a live target");
        };
        assert_eq!(shell.wait_jobs.len(), 1, "interrupted job stays owned");
        assert!(
            shell.known_async.active_entry(pid).is_some(),
            "ledger stays Active"
        );
        assert!(
            shell.known_async.consume_completed(pid).is_none(),
            "nothing consumed"
        );
    }
}
