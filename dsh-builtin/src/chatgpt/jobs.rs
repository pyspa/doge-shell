//! Managed commands an interactive `!` turn started and may outlive.
//!
//! An agent task keeps its jobs in `AgentRuntime`, which a `!` turn does not
//! have. The state lives in a process-wide slot here for the same reason
//! [`super::session`]'s conversation does: it is chat-runtime state, not shell
//! configuration, and it has to survive the window between `session::take`
//! (which empties the conversation slot) and `session::store`.
//!
//! Every job is a process group. Nothing here may leave one running that no
//! later turn can reach - see [`retain_session`] and [`cancel_all`].

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;

use crate::agent::jobs::{AgentJobs, RawRead};

/// Finished jobs kept addressable after a reap, so `job_output` can still
/// answer for a command whose result the model has not read yet.
const ARCHIVE_LIMIT: usize = 16;
/// Finished jobs left in the live table on each reap. Small, because their
/// only purpose is to be polled once more.
const KEEP_FINISHED: usize = 8;
/// Bytes one job may echo to the terminal before the live view gives up.
///
/// Without this a `yes`-shaped command would print without bound, where the
/// old synchronous path was capped by the capture it printed at the end.
const MAX_LIVE_ECHO_BYTES: usize = 256 * 1024;

/// What this registry knows that [`AgentJobs`] does not.
pub(super) struct JobMeta {
    pub(super) command: String,
    #[allow(dead_code)]
    pub(super) cwd: Option<PathBuf>,
    pub(super) started: Instant,
    /// The conversation that started this job. A turn that begins a *different*
    /// conversation cancels these, because nothing can poll them any more.
    pub(super) session_id: String,
    /// Absolute stream offsets already echoed to the terminal.
    pub(super) echoed_stdout: usize,
    pub(super) echoed_stderr: usize,
    pub(super) echoed_total: usize,
    /// Set once the echo budget is spent, so the notice prints only once.
    pub(super) echo_suppressed: bool,
    pub(super) last_poll: Option<Instant>,
    pub(super) consecutive_polls: u32,
}

#[derive(Default)]
pub(super) struct ChatJobs {
    inner: AgentJobs,
    meta: HashMap<String, JobMeta>,
    archive: VecDeque<(String, Value)>,
    /// The conversation a job started now belongs to.
    ///
    /// Kept here rather than threaded through `execute`, because the tool has
    /// no idea which conversation it is running inside and `ShellProxy` is
    /// closed to new methods. `chat_with_tools` sets it once per turn.
    current_session: String,
}

impl ChatJobs {
    pub(super) fn snapshot(&self, id: &str, offset: usize, limit: usize) -> Result<Value> {
        self.inner.snapshot(id, offset, limit)
    }

    /// The archived record of a job this registry has already reaped.
    pub(super) fn archived(&self, id: &str) -> Option<Value> {
        self.archive
            .iter()
            .find(|(archived, _)| archived == id)
            .map(|(_, value)| {
                let mut value = value.clone();
                value["archived"] = Value::Bool(true);
                value
            })
    }

    pub(super) fn cancel(&mut self, id: &str) -> Result<()> {
        self.inner.cancel(id)
    }

    pub(super) fn read_raw(
        &self,
        id: &str,
        stdout_from: usize,
        stderr_from: usize,
    ) -> Result<RawRead> {
        self.inner.read_raw(id, stdout_from, stderr_from)
    }

    /// How long a repeat poll of `id` should wait before answering.
    ///
    /// One poll is one API round trip, so a model that spins on `job_status`
    /// burns the turn's token budget on nothing. Each consecutive poll of a
    /// still-running job buys a longer floor.
    pub(super) fn poll_backoff(&mut self, id: &str) -> Duration {
        let Some(meta) = self.meta.get_mut(id) else {
            return Duration::ZERO;
        };
        let consecutive = meta.consecutive_polls;
        meta.consecutive_polls = consecutive.saturating_add(1);

        let floor = match consecutive {
            0 => return Duration::ZERO,
            1 => Duration::from_millis(1000),
            2 => Duration::from_millis(2000),
            _ => Duration::from_millis(5000),
        };

        match meta.last_poll {
            Some(last) => floor.saturating_sub(last.elapsed()),
            None => Duration::ZERO,
        }
    }

    pub(super) fn note_poll(&mut self, id: &str, still_running: bool) {
        if let Some(meta) = self.meta.get_mut(id) {
            meta.last_poll = Some(Instant::now());
            if !still_running {
                meta.consecutive_polls = 0;
            }
        }
    }

    /// New bytes to echo, capped at [`MAX_LIVE_ECHO_BYTES`].
    ///
    /// Returns `None` once this job has printed its budget, or when there is
    /// nothing new and the job is already suppressed.
    pub(super) fn take_echo(&mut self, id: &str) -> Option<(Vec<u8>, Vec<u8>, bool)> {
        self.take_echo_with_limit(id, MAX_LIVE_ECHO_BYTES)
    }

    /// Same as [`Self::take_echo`] with an injectable budget, so tests can
    /// prove the cap with tens of bytes instead of megabytes.
    ///
    /// Invariants:
    /// - the returned live bytes never push `echoed_total` past
    ///   `max_live_bytes`, even when one poll delivers more than the whole
    ///   budget;
    /// - the capture offsets (`echoed_stdout`/`echoed_stderr`) still advance
    ///   to what `read_raw` observed, so a suppressed job never re-reads the
    ///   same chunk;
    /// - `echoed_total` counts only bytes actually handed out for the
    ///   terminal, never bytes discarded from the live view.
    fn take_echo_with_limit(
        &mut self,
        id: &str,
        max_live_bytes: usize,
    ) -> Option<(Vec<u8>, Vec<u8>, bool)> {
        let (stdout_from, stderr_from) = {
            let meta = self.meta.get(id)?;
            (meta.echoed_stdout, meta.echoed_stderr)
        };
        let raw = self.inner.read_raw(id, stdout_from, stderr_from).ok()?;
        let meta = self.meta.get_mut(id)?;
        // Advance to what was observed even when nothing below is echoed:
        // otherwise a suppressed job would re-read the same huge chunk on
        // every poll.
        meta.echoed_stdout = raw.stdout_next;
        meta.echoed_stderr = raw.stderr_next;

        if meta.echo_suppressed {
            return None;
        }

        let remaining = max_live_bytes.saturating_sub(meta.echoed_total);
        let incoming = raw.stdout.len().saturating_add(raw.stderr.len());
        if incoming <= remaining {
            meta.echoed_total = meta.echoed_total.saturating_add(incoming);
            return Some((raw.stdout, raw.stderr, false));
        }

        // Over budget: spend what is left on stdout first, then stderr, and
        // discard the rest from the live view only. The capture ring keeps
        // everything; only the terminal stops here.
        let take_stdout = raw.stdout.len().min(remaining);
        let take_stderr = raw.stderr.len().min(remaining - take_stdout);
        let mut live_stdout = Vec::with_capacity(take_stdout);
        live_stdout.extend_from_slice(&raw.stdout[..take_stdout]);
        let mut live_stderr = Vec::with_capacity(take_stderr);
        live_stderr.extend_from_slice(&raw.stderr[..take_stderr]);
        meta.echoed_total = meta
            .echoed_total
            .saturating_add(live_stdout.len().saturating_add(live_stderr.len()));
        meta.echo_suppressed = true;
        Some((live_stdout, live_stderr, true))
    }

    /// Write pending live output to the given writers, for tests.
    ///
    /// Production uses [`echo_pending`], which locks the real stdio and calls
    /// this with [`MAX_LIVE_ECHO_BYTES`]. Tests pass `Vec<u8>` writers and a
    /// tiny budget so no test ever writes megabytes to the CI log.
    #[cfg(test)]
    fn echo_pending_to_with_limit(
        &mut self,
        id: &str,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
        max_live_bytes: usize,
    ) {
        let echo = self.take_echo_with_limit(id, max_live_bytes);
        write_echo(echo, stdout, stderr);
    }

    fn echo_pending_to(&mut self, id: &str, stdout: &mut dyn Write, stderr: &mut dyn Write) {
        let echo = self.take_echo(id);
        write_echo(echo, stdout, stderr);
    }

    fn reap(&mut self) {
        for (id, archived) in self.inner.reap_finished(KEEP_FINISHED) {
            self.meta.remove(&id);
            self.archive.push_back((id, archived));
            while self.archive.len() > ARCHIVE_LIMIT {
                self.archive.pop_front();
            }
        }
    }
}

/// Shared writer for both echo paths, so the suppression notice is spelled
/// once and the injected-writer tests exercise the production formatting.
fn write_echo(
    echo: Option<(Vec<u8>, Vec<u8>, bool)>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) {
    let Some((stdout_bytes, stderr_bytes, first_suppression)) = echo else {
        return;
    };
    if !stdout_bytes.is_empty() {
        let _ = stdout.write_all(&stdout_bytes);
        let _ = stdout.flush();
    }
    if !stderr_bytes.is_empty() {
        let _ = stderr.write_all(&stderr_bytes);
        let _ = stderr.flush();
    }
    if first_suppression {
        let _ = writeln!(
            stderr,
            "\x1b[2m(live output suppressed; the assistant still receives the last 1MiB)\x1b[0m"
        );
        let _ = stderr.flush();
    }
}

static CHAT_JOBS: LazyLock<Mutex<ChatJobs>> = LazyLock::new(|| Mutex::new(ChatJobs::default()));

/// The registry, recovering from a panic that happened while it was held.
///
/// Same reasoning as [`super::session`]'s slot: the jobs themselves live in
/// worker threads, so a poisoned lock means some *other* thread panicked while
/// this guard was out, not that the table is half-written. Propagating the
/// poison would strand every running process group.
fn slot() -> MutexGuard<'static, ChatJobs> {
    CHAT_JOBS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) fn with<R>(f: impl FnOnce(&mut ChatJobs) -> R) -> R {
    f(&mut slot())
}

/// Name the conversation that later [`start`] calls belong to.
pub(super) fn set_session(session_id: &str) {
    slot().current_session = session_id.to_string();
}

/// Start `command` as a managed job owned by the current conversation.
pub(super) fn start(
    builder: Command,
    command_line: &str,
    cwd: Option<PathBuf>,
    timeout: Duration,
) -> Result<String> {
    let mut jobs = slot();
    // Before the ceiling is consulted, not after: `start` counts finished jobs
    // too, so a long session would otherwise stop at its thirty-third command.
    jobs.reap();

    let id = jobs.inner.start(builder, timeout, None)?;
    let session_id = jobs.current_session.clone();
    jobs.meta.insert(
        id.clone(),
        JobMeta {
            command: command_line.to_string(),
            cwd,
            started: Instant::now(),
            session_id,
            echoed_stdout: 0,
            echoed_stderr: 0,
            echoed_total: 0,
            echo_suppressed: false,
            last_poll: None,
            consecutive_polls: 0,
        },
    );
    Ok(id)
}

/// How many jobs are still running, for the per-round banner.
pub(super) fn running_count() -> usize {
    let jobs = slot();
    running_ids(&jobs).len()
}

/// One running job, before anyone decides how to name it.
///
/// The id is the full one. A person reading `chat_status` wants the short
/// form, but the model is told to pass the id straight to `job_status`, and
/// `AgentJobs` looks it up by exact match - handing over a prefix produced
/// "unknown job" in exactly the cases where the notice was the model's only
/// source of the id.
pub(super) struct RunningJob {
    pub(super) id: String,
    pub(super) command: String,
    pub(super) elapsed_secs: u64,
    pub(super) pid: Option<u64>,
}

fn running() -> Vec<RunningJob> {
    let jobs = slot();
    let mut running: Vec<(Instant, RunningJob)> = jobs
        .meta
        .iter()
        .filter_map(|(id, meta)| {
            let state = jobs.inner.snapshot(id, 0, 0).ok()?;
            if state["status"] != "running" {
                return None;
            }
            Some((
                meta.started,
                RunningJob {
                    id: id.clone(),
                    command: meta.command.clone(),
                    elapsed_secs: meta.started.elapsed().as_secs(),
                    pid: state["pid"].as_u64(),
                },
            ))
        })
        .collect();

    running.sort_by_key(|(started, _)| *started);
    running.into_iter().map(|(_, job)| job).collect()
}

/// One line per running job for a person: `chat_status`, and the turn-end
/// notice. Short id, because a person retypes it and can also reach the job
/// through its process group.
pub(super) fn describe_running() -> Vec<String> {
    running()
        .into_iter()
        .map(|job| {
            format!(
                "{} ({}, {}s, pid {})",
                short_id(&job.id),
                job.command,
                job.elapsed_secs,
                job.pid
                    .map_or_else(|| "?".to_string(), |pid| pid.to_string())
            )
        })
        .collect()
}

/// What the model is told at the start of a turn that inherits running jobs.
///
/// Deliberately not part of the environment snapshot: that is cached on
/// `(cwd, .git/HEAD mtime)` and a value that changes every second would
/// defeat the cache. Output is left out - the model has `job_output` for it.
pub(super) fn carried_notice() -> Option<String> {
    let running = running();
    if running.is_empty() {
        return None;
    }
    let lines: Vec<String> = running
        .iter()
        .map(|job| {
            format!(
                "- {} ({}, running for {}s)",
                job.id, job.command, job.elapsed_secs
            )
        })
        .collect();

    Some(format!(
        "Commands started earlier in this conversation are still running. Use `job_status` or \
         `job_output` with the id exactly as written to follow one; do not start the command \
         again.\n{}",
        lines.join("\n")
    ))
}

/// Write whatever a job has produced since the last look, verbatim.
///
/// Shared by the `execute` wait loop and by a `job_status`/`job_output` poll,
/// so the output of a job that outlived its wait window keeps reaching the
/// screen instead of stopping the moment the model was handed a handle.
///
/// The caller owns the cursor: `execute` runs this inside `SpinnerGuard::suspend`
/// because `indicatif` holds the bottom line there, and the polling path has no
/// spinner to get out of the way of.
pub(super) fn echo_pending(id: &str) {
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut stdout = stdout.lock();
    let mut stderr = stderr.lock();
    with(|jobs| jobs.echo_pending_to(id, &mut stdout, &mut stderr));
}

/// Cancel every job, returning how many were still running.
pub(super) fn cancel_all() -> usize {
    let mut jobs = slot();
    let running = running_ids(&jobs);
    for id in &running {
        let _ = jobs.inner.cancel(id);
    }
    jobs.meta.clear();
    jobs.reap();
    running.len()
}

/// Cancel the jobs this conversation started, leaving any other alone.
///
/// What a turn cleans up when nothing will be able to poll its jobs. Narrower
/// than [`cancel_all`] on purpose: a turn that cannot carry its own work
/// forward has no business killing another conversation's build.
pub(super) fn cancel_session(session_id: &str) -> usize {
    let mut jobs = slot();
    let mine: Vec<String> = running_ids(&jobs)
        .into_iter()
        .filter(|id| {
            jobs.meta
                .get(id)
                .is_some_and(|meta| meta.session_id == session_id)
        })
        .collect();

    for id in &mine {
        let _ = jobs.inner.cancel(id);
        jobs.meta.remove(id);
    }
    jobs.reap();
    mine.len()
}

/// Cancel the jobs of every conversation other than `session_id`.
///
/// A turn that starts a different conversation can no longer reach them: the
/// model that knew their ids is gone. Leaving the process groups alive would
/// make them unkillable through the shell.
pub(super) fn retain_session(session_id: &str) -> usize {
    let mut jobs = slot();
    let stale: Vec<String> = running_ids(&jobs)
        .into_iter()
        .filter(|id| {
            jobs.meta
                .get(id)
                .is_some_and(|meta| meta.session_id != session_id)
        })
        .collect();

    for id in &stale {
        let _ = jobs.inner.cancel(id);
        jobs.meta.remove(id);
    }
    jobs.reap();
    stale.len()
}

/// Kill every managed command before the shell exits.
///
/// `CHAT_JOBS` is a `LazyLock`, which is never dropped, so `AgentJobs`' own
/// `Drop` never runs. Without this the process groups outlive the shell.
pub fn shutdown() {
    let mut jobs = slot();
    jobs.inner.cancel_all();
    jobs.meta.clear();
}

fn running_ids(jobs: &ChatJobs) -> Vec<String> {
    jobs.meta
        .keys()
        .filter(|id| {
            jobs.inner
                .snapshot(id, 0, 0)
                .is_ok_and(|state| state["status"] == "running")
        })
        .cloned()
        .collect()
}

/// A job id short enough to read, long enough to type back.
pub(super) fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

#[cfg(test)]
mod live_echo_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry is process-wide and `cancel_all`/`shutdown` reach every
    /// job in it, so these tests must not run beside anything that starts one.
    ///
    /// Deliberately the *same* lock the `execute` tests take rather than one of
    /// its own: two locks would serialize each group internally while still
    /// letting a `shutdown()` here kill a job an `execute` test was waiting on.
    fn guard() -> MutexGuard<'static, ()> {
        let guard = crate::chatgpt::tool::execute::tests::env_lock();
        shutdown();
        guard
    }

    fn sleeper(session: &str) -> String {
        set_session(session);
        let mut builder = Command::new("sh");
        builder.args(["-c", "sleep 30"]);
        start(builder, "sleep 30", None, Duration::from_secs(60)).unwrap()
    }

    /// A turn that starts a different conversation leaves the previous one's
    /// jobs unreachable - the model that knew their ids is gone - so they have
    /// to be stopped rather than left as process groups nobody can name.
    #[test]
    fn a_session_mismatch_cancels_the_jobs_of_the_previous_conversation() {
        let _lock = guard();

        let old = sleeper("conversation-a");
        let kept = sleeper("conversation-b");

        assert_eq!(retain_session("conversation-b"), 1);

        assert!(
            with(|jobs| jobs.snapshot(&old, 0, 0)).is_err_or_not_running(),
            "the orphaned job survived"
        );
        assert_eq!(
            with(|jobs| jobs.snapshot(&kept, 0, 0)).unwrap()["status"],
            "running"
        );

        shutdown();
    }

    /// A turn that cannot carry its own work forward cleans up after itself,
    /// not after everyone. The epilogue is shared with `agent run`, whose jobs
    /// live in its own `AgentRuntime` - so a failing task used to SIGKILL the
    /// build an interactive `!` had going.
    #[test]
    fn a_turn_cleaning_up_leaves_another_conversations_jobs_alone() {
        let _lock = guard();

        let mine = sleeper("conversation-a");
        let theirs = sleeper("conversation-b");

        assert_eq!(cancel_session("conversation-a"), 1);

        assert!(
            with(|jobs| jobs.snapshot(&mine, 0, 0)).is_err_or_not_running(),
            "own job survived its cleanup"
        );
        assert_eq!(
            with(|jobs| jobs.snapshot(&theirs, 0, 0)).unwrap()["status"],
            "running",
            "another conversation's job was killed"
        );

        shutdown();
    }

    /// Small readability shim: a cancelled job may be gone from the table or
    /// present with a terminal status, and both mean "not running".
    trait NotRunning {
        fn is_err_or_not_running(&self) -> bool;
    }

    impl NotRunning for Result<Value> {
        fn is_err_or_not_running(&self) -> bool {
            match self {
                Err(_) => true,
                Ok(state) => state["status"] != "running",
            }
        }
    }

    #[test]
    fn shutdown_cancels_everything() {
        let _lock = guard();

        sleeper("conversation-a");
        sleeper("conversation-a");
        assert_eq!(running_count(), 2);

        shutdown();

        assert_eq!(running_count(), 0);
        assert!(describe_running().is_empty());
    }

    /// The banner and `chat_status` read the same list, and a finished job is
    /// not something a person can still stop.
    #[test]
    fn a_finished_job_is_not_reported_as_running() {
        let _lock = guard();

        set_session("conversation-a");
        let mut builder = Command::new("sh");
        builder.args(["-c", "true"]);
        let id = start(builder, "true", None, Duration::from_secs(60)).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while with(|jobs| jobs.snapshot(&id, 0, 0)).unwrap()["status"] == "running" {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(running_count(), 0);
        assert!(carried_notice().is_none());
    }

    /// The model is told to pass the id straight to `job_status`, and
    /// `AgentJobs` looks it up by exact match - so the notice has to carry the
    /// full id even though `chat_status` shows the short one. A prefix here
    /// produced "unknown job" in exactly the cases where the notice was the
    /// model's only source of the id.
    #[test]
    fn the_carried_notice_names_a_job_by_an_id_that_can_be_polled() {
        let _lock = guard();

        let id = sleeper("conversation-a");
        let notice = carried_notice().expect("a running job is announced");

        assert!(
            notice.contains(&id),
            "notice carries only a prefix: {notice}"
        );
        // And the id in it really resolves.
        assert_eq!(
            with(|jobs| jobs.snapshot(&id, 0, 0)).unwrap()["status"],
            "running"
        );

        // The human-facing list is the one that abbreviates.
        let described = describe_running();
        assert!(described[0].starts_with(&short_id(&id)));
        assert!(!described[0].contains(&id));

        shutdown();
    }

    /// One poll is one API round trip, so a model watching a build must not be
    /// able to spin. The first poll is free; the ones after it are not.
    #[test]
    fn repeat_polls_of_a_running_job_buy_a_growing_floor() {
        let _lock = guard();

        let id = sleeper("conversation-a");

        assert_eq!(with(|jobs| jobs.poll_backoff(&id)), Duration::ZERO);
        with(|jobs| jobs.note_poll(&id, true));
        assert!(with(|jobs| jobs.poll_backoff(&id)) > Duration::ZERO);
        with(|jobs| jobs.note_poll(&id, true));
        assert!(with(|jobs| jobs.poll_backoff(&id)) >= Duration::from_millis(1500));

        // Finishing resets it: the next command's first poll is free again.
        with(|jobs| jobs.note_poll(&id, false));
        assert_eq!(with(|jobs| jobs.poll_backoff(&id)), Duration::ZERO);

        shutdown();
    }
}
