//! The in-session half of "two drivers, one tick".
//!
//! Spawned once per interactive session and aborted when it ends — so a job
//! registered with `cron add` starts firing immediately, with nothing extra
//! to install, for as long as a dsh session is actually open. A wall-clock
//! job that must also fire while nobody is logged in still needs the other
//! half, the external tick `cron setup` prints the unit for.
//!
//! Unlike `sched`'s old runner, this task does not execute anything itself —
//! it only calls the same [`tick::run_once`] an external tick does, which
//! claims due jobs and hands each to its own spawned process. What happened
//! is read back from the store (`cron history`, `cron status`), not reported
//! over a channel to this task, because the process that ran it may not be
//! this one. The one exception is [`dsh_types::cron::job::CronHealth`]: this
//! loop refreshes it after every scan so the status line
//! (`dsh/src/repl/status_line.rs`) has something to show without doing any
//! I/O of its own.
//!
//! # A known gap
//!
//! `cron run` (without `--now`) only sets a job's `next_run_at` to now; this
//! loop notices on its *next* scan, which is at most [`MAX_IDLE_SECS`] away.
//! `cron run --now` exists precisely for "I want to see it go this instant"
//! and does not depend on this task at all. A wake-on-write channel would
//! close this gap but needs a handle threaded through every mutating `cron`
//! subcommand; left for later rather than widening every handler's signature
//! for a delay whose worst case is one minute.
//!
//! Per-run REPL notices (an above-prompt line, a desktop notification when
//! `--on` says a run is worth surfacing) are also not yet wired: a run
//! finishes in a different process than this one, so reproducing `sched`'s
//! old channel-based notice needs its own watermark-and-poll design rather
//! than a straightforward port. Left for later; `cron history` is the only
//! way to see a run's outcome for now.

use super::store::SqliteCronStore;
use super::tick;
use dsh_builtin::shell_capabilities::CronStore;
use dsh_types::cron::job::RunTrigger;
use std::sync::Arc;
use std::time::Duration;

/// Upper bound between scans - long enough that two open sessions and an
/// external tick are not all polling the database every second, short enough
/// that `MIN_INTERVAL_SECS` (5s) interval jobs still fire close to on time.
const MAX_IDLE_SECS: i64 = 60;

/// Runs until the task is aborted (on `Repl` drop). Errors from one scan
/// (a locked-out store, a transient SQLite failure) are logged and do not end
/// the loop — the next scan tries again.
pub async fn cron_runner_task(
    store: Arc<SqliteCronStore>,
    health: Arc<parking_lot::RwLock<dsh_types::cron::job::CronHealth>>,
) {
    let owner = tick::owner_id();
    activate_startup_jobs(&store);
    refresh_health(&store, &health);
    loop {
        let now = chrono::Utc::now().timestamp();
        let sleep_secs = match store.next_due_at() {
            Ok(Some(at)) => (at - now).clamp(1, MAX_IDLE_SECS),
            Ok(None) => MAX_IDLE_SECS,
            Err(error) => {
                tracing::warn!("cron: could not read the next due time: {error}");
                MAX_IDLE_SECS
            }
        };
        tokio::time::sleep(Duration::from_secs(sleep_secs as u64)).await;

        let now = chrono::Utc::now().timestamp();
        if let Err(error) =
            tick::run_once(&store, &owner, tick::MAX_PER_TICK, RunTrigger::Session, now)
        {
            tracing::warn!("cron: session scan failed: {error}");
        }
        refresh_health(&store, &health);
    }
}

/// Arms every enabled, unblocked `@reboot` job (`Schedule::AtStartup`) so
/// this session's first scan claims and runs it.
///
/// `next_run_at` is permanently `NULL` for this schedule kind (see
/// `clock::next_run_at`), and the due-job query only ever looks at rows
/// where it is set - so without this, nothing in the ordinary claim path
/// could ever make one fire, and `@reboot` would be silently inert forever.
/// A session start is the only stand-in this design has for "the machine
/// started" (there is no persistent daemon to notice an actual reboot), so
/// this runs once per session rather than once per boot; an external
/// `cron tick` has no comparable moment and does not do this.
fn activate_startup_jobs(store: &SqliteCronStore) {
    let now = chrono::Utc::now().timestamp();
    let jobs = match store.list() {
        Ok(jobs) => jobs,
        Err(error) => {
            tracing::warn!("cron: could not list jobs to arm @reboot schedules: {error}");
            return;
        }
    };
    for job in jobs {
        if job.paused || job.blocked {
            continue;
        }
        if !matches!(job.schedule, dsh_types::schedule::Schedule::AtStartup) {
            continue;
        }
        if let Err(error) = store.trigger(&job.name, now) {
            tracing::warn!("cron: could not arm @reboot job {}: {error}", job.name);
        }
    }
}

/// Re-reads the cheap aggregate counts and publishes them for the status
/// line. A failure here is quiet and simply leaves the previous numbers in
/// place — a stale status line is a much smaller problem than a panicking
/// background task.
fn refresh_health(
    store: &SqliteCronStore,
    health: &Arc<parking_lot::RwLock<dsh_types::cron::job::CronHealth>>,
) {
    let now = chrono::Utc::now().timestamp();
    match store.health(now) {
        Ok(fresh) => *health.write() = fresh,
        Err(error) => tracing::warn!("cron: could not refresh status: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::cron::job::{CronJobSpec, JobKind};
    use dsh_types::schedule::{NotifyPolicy, parse_schedule};
    use std::collections::HashMap;

    fn store() -> (tempfile::TempDir, Arc<SqliteCronStore>) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteCronStore::open(&dir.path().join("cron")).unwrap();
        (dir, Arc::new(store))
    }

    fn spec(name: &str) -> CronJobSpec {
        CronJobSpec {
            name: name.to_string(),
            schedule: parse_schedule("1h").unwrap(),
            schedule_spec: "1h".to_string(),
            kind: JobKind::Sh,
            command: "true".to_string(),
            agent: None,
            cwd: "/tmp".to_string(),
            notify: NotifyPolicy::default(),
            timeout_secs: 60,
            catchup_secs: 3_600,
            paused: false,
        }
    }

    /// The real path a session takes: a job registered with `cron add` before
    /// the interactive session existed still starts firing the moment one
    /// opens, with nothing else installed.
    #[tokio::test]
    async fn a_job_due_when_the_session_opens_gets_picked_up() {
        let (_dir, store) = store();
        let now = chrono::Utc::now().timestamp();
        store
            .create(&spec("probe"), &HashMap::new(), now, false)
            .unwrap();
        store.trigger("probe", now).unwrap();

        let health = Arc::new(parking_lot::RwLock::new(
            dsh_types::cron::job::CronHealth::default(),
        ));
        let handle = tokio::spawn(cron_runner_task(Arc::clone(&store), Arc::clone(&health)));

        let picked_up = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let job = store.get("probe").unwrap();
                if job.running || job.run_count > 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;

        handle.abort();
        picked_up.expect("the session runner never claimed the due job");
    }

    /// `@reboot` (`Schedule::AtStartup`) carries a permanently `NULL`
    /// `next_run_at`, so nothing about the ordinary due-job query can ever
    /// pick it up - unlike the test above, this one does *not* call
    /// `store.trigger` itself; the runner starting is what has to arm it.
    #[tokio::test]
    async fn a_reboot_job_fires_once_when_the_session_starts() {
        let (_dir, store) = store();
        let now = chrono::Utc::now().timestamp();
        let mut reboot_spec = spec("on-boot");
        reboot_spec.schedule = parse_schedule("@reboot").unwrap();
        reboot_spec.schedule_spec = "@reboot".to_string();
        store
            .create(&reboot_spec, &HashMap::new(), now, false)
            .unwrap();
        assert_eq!(store.get("on-boot").unwrap().next_run_at, None);

        let health = Arc::new(parking_lot::RwLock::new(
            dsh_types::cron::job::CronHealth::default(),
        ));
        let handle = tokio::spawn(cron_runner_task(Arc::clone(&store), Arc::clone(&health)));

        let picked_up = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let job = store.get("on-boot").unwrap();
                if job.running || job.run_count > 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;

        handle.abort();
        picked_up.expect("the runner never armed and claimed the @reboot job");
        // Firing once must not turn into firing every scan thereafter: a
        // fresh `next_run_at` of `NULL` is the schedule's steady state.
        assert_eq!(store.get("on-boot").unwrap().next_run_at, None);
    }

    /// Dropping (aborting) the task must not leave the runner's own claim
    /// dangling forever - the lease-reap path is what recovers it, and this
    /// just confirms the task is actually abortable mid-sleep.
    #[tokio::test]
    async fn the_task_can_be_aborted_while_idle() {
        let (_dir, store) = store();
        let health = Arc::new(parking_lot::RwLock::new(
            dsh_types::cron::job::CronHealth::default(),
        ));
        let handle = tokio::spawn(cron_runner_task(store, health));
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.abort();
        assert!(handle.await.unwrap_err().is_cancelled());
    }
}
