//! The `wait` builtin: wait for known async jobs and report their status.
//!
//! Operands are PIDs or `%`-prefixed job specs (`wait`, `wait PID`,
//! `wait PID1 PID2 ...`, `wait %1`, `wait %+`, `wait %-`, `wait %%`), plus
//! `wait -n` to wait for the next completion among a target set and
//! `wait -p VAR` to publish the completed job's canonical associated PID to
//! a shell variable. Bare numbers are always PIDs here, never job numbers:
//! `wait 1` means PID 1, only `wait %1` means job 1. `wait -f` remains out
//! of scope and is rejected. Statuses come from `Job::final_exit_status()`
//! using the frozen pipeline policy via the completed-job finalizer; the
//! ledger is consumed only here, never by reconciliation (`jobs`, notices,
//! `fg`/`bg`).
//!
//! `wait -n` never uses `waitpid(-1)`: it polls only the canonical PID set
//! of its resolved targets, so unrelated children (process substitution
//! helpers, detached children, agent children) keep their statuses.

use super::{JobSpec, parse_percent_job_spec, resolve_active_job_spec};
use crate::environment::variables::is_valid_shell_var_name;
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

/// Outcome of waiting for a single PID, identity-aware.
///
/// A real child that exits 127 and an unknown PID both surface shell status
/// 127, but only the former carries a [`WaitCompletion`]: `-p` assignment
/// must be able to tell them apart without guessing from the status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitOneOutcome {
    /// A known child/job produced this status; carries its identity.
    Completed(WaitCompletion),
    /// The wait command reports this status, but no real child completion
    /// produced it (unknown PID, invalid operand, unknown jobspec, stale
    /// `Active` ledger entry).
    NoCompletion(i32),
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
/// `wait -p` publishes the canonical associated PID. `job_id`/`source`
/// remain internal identity metadata.
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

/// One completed wait-any result: status plus the identity metadata
/// `wait -p VAR` publishes. Never just a bare status code.
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

/// The parsed `wait` command line: `-n` mode, `-p` destination, operands.
struct WaitInvocation {
    next: bool,
    assign_to: Option<String>,
    operands: Vec<String>,
}

/// One `wait` invocation's user-visible result: the `$?` status plus the
/// identity of the known completion that actually produced it, if any.
///
/// `completion` is the final returned status's source only: a later
/// `NoCompletion` resets it to `None`, so `wait -p` never publishes a stale
/// PID when the invocation as a whole ends on an unknown target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WaitCommandOutcome {
    status: i32,
    completion: Option<WaitCompletion>,
}

/// `wait` entry point: bridge the async wait onto a runtime shared with
/// `fg`/`jobs`.
pub fn execute_wait(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<i32> {
    super::block_on_job_control_future(wait_async(shell, ctx, argv))?
}

/// Async body of `wait`.
async fn wait_async(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<i32> {
    let invocation = parse_wait_invocation(&argv)?;
    prepare_wait_assignment(shell, invocation.assign_to.as_deref())?;
    let outcome = if invocation.next {
        wait_next_command(shell, ctx, &invocation.operands).await?
    } else if invocation.operands.is_empty() {
        wait_bare_command(shell, ctx).await?
    } else {
        wait_sequential_command(shell, ctx, &invocation.operands).await?
    };
    publish_wait_assignment(shell, invocation.assign_to.as_deref(), outcome.completion);
    Ok(outcome.status)
}

/// Bare `wait`: every known async PID, status always 0 (individual child
/// failures do not become the builtin's status). All entries are consumed.
/// Never publishes an identity: with `-p` and no operands the destination
/// stays unset.
async fn wait_bare_command(shell: &mut Shell, ctx: &Context) -> Result<WaitCommandOutcome> {
    for pid in shell.known_async.known_pids() {
        match wait_one(shell, ctx, pid).await? {
            WaitOneOutcome::Completed(_) | WaitOneOutcome::NoCompletion(_) => {}
            WaitOneOutcome::Interrupted => {
                return Ok(WaitCommandOutcome {
                    status: 130,
                    completion: None,
                });
            }
        }
    }
    Ok(WaitCommandOutcome {
        status: 0,
        completion: None,
    })
}

/// Sequential `wait`: each operand in order, the last status wins.
/// Unknown targets report 127 without stopping the remaining operands.
/// Only the identity behind the final returned status is published.
async fn wait_sequential_command(
    shell: &mut Shell,
    ctx: &Context,
    operands: &[String],
) -> Result<WaitCommandOutcome> {
    let mut result = WaitCommandOutcome {
        status: 0,
        completion: None,
    };
    for operand in operands {
        match wait_operand(shell, ctx, operand).await? {
            WaitOneOutcome::Completed(completion) => {
                result.status = completion.status;
                result.completion = Some(completion);
            }
            WaitOneOutcome::NoCompletion(status) => {
                result.status = status;
                result.completion = None;
            }
            WaitOneOutcome::Interrupted => {
                return Ok(WaitCommandOutcome {
                    status: 130,
                    completion: None,
                });
            }
        }
    }
    Ok(result)
}

/// Validate (already done by the parser; re-checked here so direct callers
/// cannot skip it) and logically `unset` the `-p` destination before
/// waiting. Uses [`Environment::unset_shell_var`](crate::environment::Environment::unset_shell_var):
/// value and export bit are both removed, matching `unset VAR` semantics.
fn prepare_wait_assignment(shell: &mut Shell, assign_to: Option<&str>) -> Result<()> {
    let Some(name) = assign_to else {
        return Ok(());
    };
    if !is_valid_shell_var_name(name) {
        anyhow::bail!("wait: invalid variable name: {name}");
    }
    shell.environment.write().unset_shell_var(name);
    Ok(())
}

/// Publish the invocation's final identity exactly once, after the outcome
/// is selected — never incrementally mid-loop, so an interrupt leaves the
/// destination unset rather than holding a stale PID. `None` publishes
/// nothing (unknown/stale target, bare wait, interrupt, no targets).
fn publish_wait_assignment(
    shell: &mut Shell,
    assign_to: Option<&str>,
    completion: Option<WaitCompletion>,
) {
    let (Some(name), Some(completed)) = (assign_to, completion) else {
        return;
    };
    shell
        .environment
        .write()
        .set_shell_var(name.to_string(), completed.pid.as_raw().to_string());
}

/// Parse the `wait` command line: `-n` mode, `-p VAR` destination, `--`
/// end-of-options, operands.
///
/// Bundled short flags (`-nn`, `-np VAR`) work bash/getopt-like: every flag
/// must be a supported one. `-p` takes a value (the next argv element, or
/// the remainder of the same element); a repeated `-p` overwrites the
/// previous destination. `-f` and any other `-` option stay rejected (usage
/// error, exit 1 via the builtin wrapper). A lone `-` is an operand, never
/// an option.
fn parse_wait_invocation(argv: &[String]) -> Result<WaitInvocation> {
    let mut next = false;
    let mut assign_to: Option<String> = None;
    let mut operands = Vec::new();
    let mut end_of_options = false;
    let rest = argv.get(1..).unwrap_or(&[]);
    let mut index = 0;
    while index < rest.len() {
        let operand = &rest[index];
        index += 1;
        if !end_of_options && operand == "--" {
            end_of_options = true;
            continue;
        }
        if !end_of_options
            && operand != "-"
            && let Some(flags) = operand.strip_prefix('-')
        {
            // `flags` is never empty here: `operand != "-"` above rules out
            // the only input `strip_prefix` maps to `Some("")`.
            let chars: Vec<char> = flags.chars().collect();
            let mut flag_index = 0;
            while flag_index < chars.len() {
                match chars[flag_index] {
                    'n' => {
                        next = true;
                        flag_index += 1;
                    }
                    'p' => {
                        let trailing: String = chars[flag_index + 1..].iter().collect();
                        let value = if !trailing.is_empty() {
                            trailing
                        } else {
                            match rest.get(index) {
                                Some(next_arg) => {
                                    index += 1;
                                    next_arg.clone()
                                }
                                None => {
                                    anyhow::bail!("wait: option -p requires a variable name")
                                }
                            }
                        };
                        if !is_valid_shell_var_name(&value) {
                            anyhow::bail!("wait: invalid variable name: {value}")
                        }
                        assign_to = Some(value);
                        flag_index = chars.len();
                    }
                    _ => {
                        // No pre-bail write: the builtin wrapper prints the
                        // `Err` once.
                        anyhow::bail!("wait: unsupported option: {operand}")
                    }
                }
            }
            // Every flag above was consumed as an option (`-n` and/or
            // `-p VAR`); nothing is pushed as an operand.
            continue;
        }
        operands.push(operand.clone());
    }
    Ok(WaitInvocation {
        next,
        assign_to,
        operands,
    })
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
/// targets report 127 and let the remaining operands run. Never invents a
/// [`WaitCompletion`] for a PID this shell does not own.
async fn wait_operand(shell: &mut Shell, ctx: &Context, operand: &str) -> Result<WaitOneOutcome> {
    let parsed = match parse_wait_operand(operand) {
        Ok(parsed) => parsed,
        Err(WaitOperandError::Invalid) => {
            let _ = ctx.write_stderr(&format!("wait: '{operand}': not a pid or job spec"));
            return Ok(WaitOneOutcome::NoCompletion(127));
        }
    };
    match resolve_wait_operand(shell, &parsed) {
        Some(target) => wait_target(shell, ctx, target).await,
        None => {
            let _ = ctx.write_stderr(&format!("wait: '{operand}': no such job"));
            Ok(WaitOneOutcome::NoCompletion(127))
        }
    }
}

/// Wait for one resolved target: the canonical single-target path.
///
/// An active table job is taken and termination-waited; an
/// already-completed ledger entry needs no OS wait; anything else is
/// unknown (127) and never touches `waitpid`. Ledger metadata is captured
/// before consuming: after `consume_completed` the entry is gone and
/// `job_id_for_pid` can no longer answer.
async fn wait_target(
    shell: &mut Shell,
    ctx: &Context,
    target: ResolvedWaitTarget,
) -> Result<WaitOneOutcome> {
    if let Some(index) = shell
        .wait_jobs
        .iter()
        .position(|job| job.pid == Some(target.pid))
    {
        return wait_active_job(shell, index).await;
    }
    if shell.known_async.completed_status(target.pid).is_some() {
        let job_id = target
            .job_id
            .or_else(|| shell.known_async.job_id_for_pid(target.pid));
        let status = shell
            .known_async
            .consume_completed(target.pid)
            .expect("completed status peeked above must still be consumable");
        debug!(
            "wait: pid {} already completed with status {status}",
            target.pid
        );
        return Ok(WaitOneOutcome::Completed(WaitCompletion {
            pid: target.pid,
            job_id,
            status,
        }));
    }
    if shell.known_async.remove(target.pid).is_some() {
        // Active ledger entry but no table job: stale ownership that can
        // never complete. Drop it rather than blocking forever.
        debug!(
            "wait: pid {} has a stale active ledger entry, dropping it",
            target.pid
        );
    }
    let _ = ctx.write_stderr(&format!(
        "wait: '{}': not a child of this shell",
        target.pid
    ));
    Ok(WaitOneOutcome::NoCompletion(127))
}

/// Wait for one PID: builds the target metadata, then delegates to the
/// canonical [`wait_target`] path so bare `wait` shares the same
/// active/completed/unknown semantics.
async fn wait_one(shell: &mut Shell, ctx: &Context, pid: Pid) -> Result<WaitOneOutcome> {
    let job_id = shell
        .wait_jobs
        .iter()
        .find(|job| job.pid == Some(pid))
        .map(|job| job.job_id)
        .or_else(|| shell.known_async.job_id_for_pid(pid));
    wait_target(
        shell,
        ctx,
        ResolvedWaitTarget {
            pid,
            job_id,
            source: WaitTargetSource::Pid,
        },
    )
    .await
}

/// Termination-wait a table job under temporary ownership (`fg` model).
async fn wait_active_job(shell: &mut Shell, index: usize) -> Result<WaitOneOutcome> {
    let mut job = shell.wait_jobs.remove(index);
    let pid_hint = job.pid;
    match wait_for_termination(&mut job).await {
        Ok(JobWaitOutcome::Completed) => {
            let job = finalize_completed_job(shell, job, FinalizeDrain::ToEof).await?;
            let status = final_exit_status(&job).ok_or_else(|| {
                anyhow::anyhow!("wait: completed job {} has no final status", job.job_id)
            })?;
            // Archive first, then consume: the status reaches the caller
            // exactly once.
            let pid = job
                .pid
                .or(pid_hint)
                .expect("active wait target always has an associated PID");
            let raw_pid = pid.as_raw();
            let consumed = job
                .pid
                .and_then(|pid| shell.known_async.consume_completed(pid));
            debug!(
                "wait: pid {raw_pid} completed with status {status} (ledger consumed: {})",
                consumed.is_some()
            );
            Ok(WaitOneOutcome::Completed(WaitCompletion {
                pid,
                job_id: Some(job.job_id),
                status,
            }))
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
async fn wait_next_command(
    shell: &mut Shell,
    ctx: &Context,
    operands: &[String],
) -> Result<WaitCommandOutcome> {
    let targets = resolve_wait_next_targets(shell, ctx, operands);
    if targets.is_empty() {
        // No waitable target: unlike bare `wait` (which reports 0 with no
        // known jobs), `wait -n` reports 127. Unknown-operand diagnostics
        // were already printed during resolution.
        return Ok(WaitCommandOutcome {
            status: 127,
            completion: None,
        });
    }
    match wait_next(shell, targets).await? {
        WaitNextOutcome::Completed(completion) => Ok(WaitCommandOutcome {
            status: completion.status,
            completion: Some(completion),
        }),
        WaitNextOutcome::Interrupted => Ok(WaitCommandOutcome {
            status: 130,
            completion: None,
        }),
        WaitNextOutcome::NoTargets => Ok(WaitCommandOutcome {
            status: 127,
            completion: None,
        }),
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
mod tests;
