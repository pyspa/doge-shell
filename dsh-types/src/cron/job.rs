//! The shapes a cron job, its runs and its incidents take on the wire.
//!
//! These live in `dsh-types` because the `cron` builtin renders them and the
//! shell's store produces them, and the two cannot see each other. Every enum
//! here has an `as_str` / `parse` pair rather than a `serde` rename, because
//! the same spelling is a SQLite column value, a `--json` field and something
//! a person types at the prompt; one function per direction keeps those three
//! from drifting.
//!
//! Durations are plain seconds, not [`std::time::Duration`], for the same
//! reason: they round-trip through an `INTEGER` column unchanged.

use crate::agent::TaskGrant;
use crate::schedule::{NotifyPolicy, Schedule};
use serde::{Deserialize, Serialize};
use std::fmt;

#[cfg(test)]
mod tests;

/// What a job runs.
///
/// The two arms diverge completely at execution time - a shell job goes
/// through `sh -c`, an agent job never touches a shell - so this is the first
/// thing the tick branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// A command line, run detached under `sh -c`.
    Sh,
    /// An unattended `agent run`, driven from a structured spec.
    Ai,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sh => "sh",
            Self::Ai => "ai",
        }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        Ok(match name {
            "sh" => Self::Sh,
            "ai" => Self::Ai,
            _ => return Err(format!("{name}: expected sh or ai")),
        })
    }
}

impl fmt::Display for JobKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The part of an agent job that does not fit in a column.
///
/// Stored as JSON in `jobs.payload`. The goal itself is **not** here - it
/// lives in `jobs.command`, the same column a shell job uses, so that "what
/// does this job do" is one lookup for both kinds.
///
/// [`TaskGrant`] is reused verbatim rather than mirrored: a cron agent job is
/// a scheduled `agent run`, and the moment the two grant shapes differ, a
/// permission means one thing on the prompt and another from the tick.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentJobSpec {
    pub grant: TaskGrant,
    /// Completion criteria, verified against recorded tool results.
    pub criteria: Vec<String>,
    /// Per-run ceiling. Each run starts a fresh task, so this is never a
    /// running total the way `agent resume` treats it.
    pub token_budget: u64,
    pub time_budget_secs: u64,
    /// Across a rolling 24 hours, over every run of this job. Without it a
    /// five-minute schedule is an unbounded bill.
    pub max_tokens_per_day: Option<u64>,
}

/// Everything needed to create a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronJobSpec {
    pub name: String,
    pub schedule: Schedule,
    /// The spelling the user typed. [`Schedule`] normalises, and showing
    /// someone `0,30 * * * *` when they wrote `*/30 * * * *` is a needless
    /// puzzle.
    pub schedule_spec: String,
    pub kind: JobKind,
    /// A command line for [`JobKind::Sh`], the goal for [`JobKind::Ai`].
    pub command: String,
    /// Present exactly when `kind` is [`JobKind::Ai`].
    pub agent: Option<AgentJobSpec>,
    pub cwd: String,
    pub notify: NotifyPolicy,
    pub timeout_secs: u64,
    /// How far behind the wall clock a missed run is still worth doing. Past
    /// this, the backlog collapses to a single run instead of firing one per
    /// slot the machine slept through.
    pub catchup_secs: u64,
    pub paused: bool,
}

/// The fields `cron edit` can change, one at a time.
///
/// `None` means "leave it alone" - distinct from a value that happens to be
/// empty, which is why this is not just another `CronJobSpec`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CronJobPatch {
    pub name: Option<String>,
    pub schedule: Option<(Schedule, String)>,
    pub command: Option<String>,
    pub agent: Option<AgentJobSpec>,
    pub cwd: Option<String>,
    pub notify: Option<NotifyPolicy>,
    pub timeout_secs: Option<u64>,
    pub catchup_secs: Option<u64>,
}

impl CronJobPatch {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// How one run ended.
///
/// `Skipped` is deliberately not a failure: another agent task holding the
/// lock, or the job's own previous run still going, are ordinary and must not
/// move `fail_count` or trip a failure notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunState {
    /// Claimed, not started. A crash leaves rows here for recovery to find.
    Queued,
    Running,
    Succeeded,
    Failed,
    Skipped,
    /// The agent asked for a permission it was not granted. Not retried.
    NeedsApproval,
    Cancelled,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::NeedsApproval => "needs-approval",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        Ok(match name {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "skipped" => Self::Skipped,
            "needs-approval" => Self::NeedsApproval,
            "cancelled" => Self::Cancelled,
            _ => return Err(format!("{name}: unknown run state")),
        })
    }

    /// Whether the run is over. `history` shows these; `runs` shows the rest.
    pub fn is_finished(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }

    /// Whether this run should count against the job's failure streak.
    pub fn counts_as_failure(self) -> bool {
        matches!(self, Self::Failed)
    }
}

impl fmt::Display for RunState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a run ended the way it did, when the state alone is not enough.
///
/// Split from [`RunState`] rather than folded into it because the retry rule
/// hangs off the reason, not the state: a `Transient` failure is worth another
/// tick, a `Config` failure is the same answer every time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunReason {
    /// Another `agent` task held the execution lock.
    AgentBusy,
    /// This job's previous run had not finished.
    StillRunning,
    /// The rolling daily token ceiling was already spent.
    BudgetExhausted,
    /// Network or provider hiccup. Retried; only a streak becomes an incident.
    Transient,
    /// Killed by the outer wall-clock deadline.
    Timeout,
    /// No API key, or a key the provider rejected outright.
    Config,
    /// The provider returned no usage, so a budget cannot be enforced.
    Provider,
    /// The child process could not be started.
    Spawn,
    /// The job's directory is gone, or now resolves somewhere else.
    RootChanged,
    /// A hook refused the turn, or ran out of its budget and failed closed.
    HookDeny,
    /// The state directory or database is unusable.
    StateUnusable,
    /// An operation's outcome is unknown; a person must confirm reality.
    Reconcile,
}

impl RunReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentBusy => "agent-busy",
            Self::StillRunning => "still-running",
            Self::BudgetExhausted => "budget-exhausted",
            Self::Transient => "transient",
            Self::Timeout => "timeout",
            Self::Config => "config",
            Self::Provider => "provider",
            Self::Spawn => "spawn",
            Self::RootChanged => "root-changed",
            Self::HookDeny => "hook-deny",
            Self::StateUnusable => "state-unusable",
            Self::Reconcile => "reconcile",
        }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        Ok(match name {
            "agent-busy" => Self::AgentBusy,
            "still-running" => Self::StillRunning,
            "budget-exhausted" => Self::BudgetExhausted,
            "transient" => Self::Transient,
            "timeout" => Self::Timeout,
            "config" => Self::Config,
            "provider" => Self::Provider,
            "spawn" => Self::Spawn,
            "root-changed" => Self::RootChanged,
            "hook-deny" => Self::HookDeny,
            "state-unusable" => Self::StateUnusable,
            "reconcile" => Self::Reconcile,
            _ => return Err(format!("{name}: unknown run reason")),
        })
    }

    /// Whether another tick is worth trying without a person intervening.
    ///
    /// The `false` arms all describe something that will answer identically
    /// next time; retrying them burns tokens and buries the real incident.
    pub fn is_worth_retrying(self) -> bool {
        matches!(
            self,
            Self::AgentBusy | Self::StillRunning | Self::Transient | Self::Timeout
        )
    }
}

impl fmt::Display for RunReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a tick did once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronRun {
    pub id: String,
    pub job_id: i64,
    pub job_name: String,
    /// The slot this run belongs to, in Unix seconds UTC. Local time would
    /// repeat an hour every autumn and let the same slot fire twice.
    pub scheduled_for: i64,
    pub state: RunState,
    pub reason: Option<RunReason>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub duration_ms: u64,
    pub exit_code: i32,
    pub timed_out: bool,
    /// Output differed from the previous run, for `--on change`.
    pub changed: bool,
    pub trigger: RunTrigger,
    /// The `agent` task this run created, for `agent show`.
    pub agent_task_id: Option<String>,
    pub tokens_used: u64,
    /// Skill proposals the run left for a person in `skill pending`.
    pub pending_skills: u32,
    /// First line of output, already masked.
    pub preview: String,
}

impl CronRun {
    pub fn succeeded(&self) -> bool {
        self.state == RunState::Succeeded
    }
}

/// What set a run going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTrigger {
    /// An interactive session's runner noticed it was due.
    Session,
    /// An external `cron tick`.
    Tick,
    /// `cron run`.
    Manual,
    /// An `@reboot` job, when a session started.
    Startup,
}

impl RunTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Tick => "tick",
            Self::Manual => "manual",
            Self::Startup => "startup",
        }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        Ok(match name {
            "session" => Self::Session,
            "tick" => Self::Tick,
            "manual" => Self::Manual,
            "startup" => Self::Startup,
            _ => return Err(format!("{name}: unknown run trigger")),
        })
    }
}

impl fmt::Display for RunTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Something only a person can clear.
///
/// The bar is deliberately high: anything a later tick could fix on its own
/// stays a failed run. An incident means the job is stuck until someone acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IncidentKind {
    /// The agent needs a grant it was not given.
    Approval,
    /// A hook asked a question. `cron edit --allow-*` cannot answer it.
    HookAsk,
    /// An operation's result is unknown and must not be replayed.
    Reconcile,
    /// The daily token ceiling is spent.
    Budget,
    /// The provider cannot support an unattended run.
    Provider,
    /// No API key.
    Config,
    /// A streak of ordinary failures long enough to stop calling it transient.
    Failing,
    /// Repeatedly could not get the agent execution lock.
    LockStarvation,
    /// A claim expired while its owner was presumed alive.
    LeaseLost,
    /// The store itself is unusable; every agent job is stopped.
    StateUnusable,
    /// The job's directory changed identity.
    RootChanged,
}

impl IncidentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::HookAsk => "hook-ask",
            Self::Reconcile => "reconcile",
            Self::Budget => "budget",
            Self::Provider => "provider",
            Self::Config => "config",
            Self::Failing => "failing",
            Self::LockStarvation => "lock-starvation",
            Self::LeaseLost => "lease-lost",
            Self::StateUnusable => "state-unusable",
            Self::RootChanged => "root-changed",
        }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        Ok(match name {
            "approval" => Self::Approval,
            "hook-ask" => Self::HookAsk,
            "reconcile" => Self::Reconcile,
            "budget" => Self::Budget,
            "provider" => Self::Provider,
            "config" => Self::Config,
            "failing" => Self::Failing,
            "lock-starvation" => Self::LockStarvation,
            "lease-lost" => Self::LeaseLost,
            "state-unusable" => Self::StateUnusable,
            "root-changed" => Self::RootChanged,
            _ => return Err(format!("{name}: unknown incident kind")),
        })
    }

    /// Whether the job stops firing until the incident is acknowledged.
    ///
    /// Only the ones a retry cannot clear. A `Failing` streak keeps trying,
    /// because the thing it is waiting for may come back on its own.
    /// `StateUnusable` is the opposite of that: a broken task store or state
    /// directory is not something the next tick can route around, and
    /// retrying it every schedule slot just repeats the same failure - its
    /// own doc comment ("every agent job is stopped") is the contract this
    /// upholds.
    pub fn blocks_the_job(self) -> bool {
        matches!(
            self,
            Self::Approval
                | Self::HookAsk
                | Self::Reconcile
                | Self::Provider
                | Self::Config
                | Self::RootChanged
                | Self::StateUnusable
        )
    }
}

impl fmt::Display for IncidentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One open or acknowledged incident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronIncident {
    pub id: i64,
    pub job_id: Option<i64>,
    pub job_name: Option<String>,
    pub opened_at: i64,
    pub acked_at: Option<i64>,
    pub kind: IncidentKind,
    /// Already masked; this is shown and notified.
    pub detail: String,
    pub agent_task_id: Option<String>,
}

impl CronIncident {
    pub fn is_open(&self) -> bool {
        self.acked_at.is_none()
    }
}

/// A job as `cron list` and `cron show` print it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronJobView {
    pub id: i64,
    pub name: String,
    pub kind: JobKind,
    pub schedule: Schedule,
    pub schedule_spec: String,
    pub command: String,
    pub agent: Option<AgentJobSpec>,
    pub cwd: String,
    pub notify: NotifyPolicy,
    pub timeout_secs: u64,
    pub catchup_secs: u64,
    pub paused: bool,
    /// Held back until an incident is acknowledged.
    pub blocked: bool,
    /// Unix seconds UTC, or `None` when nothing will fire it.
    pub next_run_at: Option<i64>,
    pub running: bool,
    pub run_count: u64,
    pub fail_count: u64,
    pub consecutive_failures: u32,
    pub last: Option<CronRun>,
}

impl CronJobView {
    /// What `cron list` prints in the state column.
    pub fn state_label(&self) -> &'static str {
        if self.blocked {
            "blocked"
        } else if self.paused {
            "paused"
        } else if self.running {
            "running"
        } else if self.consecutive_failures > 0 {
            "failing"
        } else {
            "ok"
        }
    }
}

/// The counts `cron status` and the status line show.
///
/// Cached in the environment rather than queried: the status line composes on
/// the REPL's own task and must not touch the database.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CronHealth {
    pub total: usize,
    pub running: usize,
    pub failing: usize,
    pub paused: usize,
    pub blocked: usize,
    pub open_incidents: usize,
    pub overdue: usize,
    /// The most recent finished run, for "is a tick actually arriving".
    pub last_run_at: Option<i64>,
}

/// A run the tick has claimed and is about to start.
///
/// Everything the child process needs, so that `cron run-job <uuid>` is one
/// row read and no re-derivation. The goal and the grant travel as data, never
/// as a command line: the only thing that reaches a shell parser is the UUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedRun {
    pub run_id: String,
    pub job_id: i64,
    pub job_name: String,
    pub kind: JobKind,
    /// A command line for [`JobKind::Sh`], the goal for [`JobKind::Ai`].
    pub command: String,
    pub agent: Option<AgentJobSpec>,
    pub cwd: String,
    /// The environment snapshot taken when the job was created. A job must not
    /// inherit whatever the session that happened to tick it was carrying.
    pub env: std::collections::HashMap<String, String>,
    pub timeout_secs: u64,
    pub notify: NotifyPolicy,
    pub scheduled_for: i64,
    pub trigger: RunTrigger,
    /// The previous run's output digest, for `--on change`.
    pub last_digest: Option<u64>,
}

/// How a run finished, on its way back into the store.
///
/// `changed` is absent on purpose: the store owns the comparison against
/// `jobs.last_digest`, so a caller cannot report "changed" without also
/// updating what it is next compared against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutcome {
    pub state: RunState,
    pub reason: Option<RunReason>,
    pub exit_code: i32,
    pub timed_out: bool,
    pub duration_ms: u64,
    /// Already masked by the caller. This is persisted.
    pub stdout: String,
    pub stderr: String,
    /// `None` leaves the previous digest in place, which is what a skipped run
    /// wants: it produced no output to compare next time against.
    pub digest: Option<u64>,
    pub agent_task_id: Option<String>,
    pub tokens_used: u64,
    pub pending_skills: u32,
}

impl Default for RunState {
    /// A run that never reported back is interrupted, not successful. The
    /// default matters because it is what a partially built outcome carries.
    fn default() -> Self {
        Self::Failed
    }
}

/// A claim lasts twice the job's timeout, never less than a minute.
///
/// Shorter than the run would let a second driver take a job that is still
/// going. The floor covers jobs with a very small timeout, where twice it is
/// less than the time to start a process.
///
/// Shared between the store's lease expiry (`dsh/src/cron/store/claim.rs`)
/// and an AI job's in-process watchdog (`dsh/src/cron/run_job.rs`): the
/// watchdog kills its own process a few seconds *before* this, so a hung run
/// is dead before another driver would otherwise reclaim its row out from
/// under a still-running process.
pub fn lease_secs(timeout_secs: u64) -> i64 {
    // `timeout_secs` is validated at the CLI boundary (`parse_named_duration`)
    // to fit in an `i64`, but this cast has no way to know that from here -
    // and getting it wrong the other direction (silently truncating a huge
    // timeout down to a tiny lease) is exactly the bug worth guarding
    // against, not just documenting. `try_from` plus a saturating fallback
    // means a value that *did* slip through still yields a lease at least as
    // long as intended, never shorter.
    i64::try_from(timeout_secs)
        .unwrap_or(i64::MAX)
        .saturating_mul(2)
        .max(60)
}

/// What `cron runs` and `cron history` are asking for.
///
/// One struct rather than four positional flags, because three of them are
/// booleans and a caller that swaps two would get a plausible wrong answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunQuery {
    /// A job name or id; `None` means every job.
    pub job: Option<String>,
    pub limit: usize,
    /// `history` shows finished runs; `runs` shows everything, including the
    /// ones still queued or in flight.
    pub finished_only: bool,
    pub failed_only: bool,
}

/// Which run's recorded output `cron logs` is asking for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunSelector {
    /// The most recently finished run of this job (name or id).
    Latest(String),
    /// One exact run, by its id or a unique prefix of one (git-style).
    Id(String),
}

/// One run's full recorded streams, read on demand.
///
/// Deliberately not fields on [`CronRun`]: `cron list`/`cron history` read up
/// to 200 rows at a time, and carrying two 8 KiB columns through every one of
/// them would make the common path (a table of recent runs) pay for the rare
/// one (reading a single run's full output). `cron logs` is the only caller
/// that needs the bytes, so it is the only one that asks for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    pub run: CronRun,
    pub stdout: String,
    pub stderr: String,
}
