//! The background task that scans for due work and runs it.
//!
//! Spawned once from `Repl::new` and aborted when the REPL drops, so scheduled
//! tasks live exactly as long as the interactive session.

use super::{SchedulerEvent, SharedScheduler, exec};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, mpsc::UnboundedSender};
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tracing::debug;

/// How often to look for due tasks. Intervals are at least 5s, so a 1-second
/// scan is accurate enough while staying cheap when nothing is scheduled.
const SCAN_INTERVAL: Duration = Duration::from_secs(1);

/// Ceiling on concurrently running tasks, so a burst of due work cannot fork
/// the machine to a standstill.
const MAX_PARALLEL: usize = 2;

pub async fn scheduler_task(scheduler: SharedScheduler, tx: UnboundedSender<SchedulerEvent>) {
    let permits = Arc::new(Semaphore::new(MAX_PARALLEL));
    let mut scan = interval_at(Instant::now() + SCAN_INTERVAL, SCAN_INTERVAL);
    // A blocked scan should not fire a catch-up burst afterwards.
    scan.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        scan.tick().await;

        let due = {
            let mut state = scheduler.write();
            if state.is_empty() {
                continue;
            }
            state.claim_due(std::time::Instant::now())
        };

        for task in due {
            let scheduler = Arc::clone(&scheduler);
            let tx = tx.clone();
            let permits = Arc::clone(&permits);

            tokio::spawn(async move {
                // The task is already marked running, so holding it in the
                // queue here does not let a second copy start.
                let Ok(_permit) = permits.acquire().await else {
                    scheduler.write().finish(task.id);
                    return;
                };

                debug!("sched: running '{}'", task.name);
                let outcome = exec::run(&task).await;

                let recorded = {
                    let mut state = scheduler.write();
                    let recorded = state.record(
                        task.id,
                        &outcome.stdout,
                        outcome.exit_code,
                        outcome.timed_out,
                        outcome.duration,
                    );
                    state.finish(task.id);
                    recorded
                };

                // `None` means the task was removed mid-run; drop the result.
                let Some((run, _policy, notify)) = recorded else {
                    return;
                };

                let _ = tx.send(SchedulerEvent {
                    id: task.id,
                    name: task.name,
                    command: task.command,
                    cwd: task.cwd,
                    stdout: outcome.stdout,
                    stderr: outcome.stderr,
                    exit_code: outcome.exit_code,
                    duration: outcome.duration,
                    timed_out: outcome.timed_out,
                    changed: run.changed,
                    notify,
                });
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::SchedulerState;
    use dsh_types::schedule::{NotifyPolicy, SchedTaskSpec, parse_interval};
    use std::collections::HashMap;

    fn spec(command: &str) -> SchedTaskSpec {
        SchedTaskSpec {
            name: "probe".to_string(),
            // The floor is 5s, so tests use `trigger` to make a task due
            // immediately rather than waiting out an interval.
            interval: parse_interval("1h").unwrap(),
            command: command.to_string(),
            cwd: "/tmp".to_string(),
            notify: NotifyPolicy::Always,
            timeout: Duration::from_secs(10),
        }
    }

    /// Registers a task, triggers it, and waits for the runner to report back.
    async fn run_once(command: &str) -> SchedulerEvent {
        let scheduler = SchedulerState::shared();
        let env = HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]);
        {
            let mut state = scheduler.write();
            state.add(spec(command), env).unwrap();
            state.trigger("probe").unwrap();
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(scheduler_task(Arc::clone(&scheduler), tx));

        let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("runner did not report within 10s")
            .expect("channel closed");
        handle.abort();
        event
    }

    #[tokio::test]
    async fn runs_a_due_task_and_reports_the_result() {
        let event = run_once("echo scheduled").await;
        assert_eq!(event.name, "probe");
        assert_eq!(event.stdout.trim(), "scheduled");
        assert_eq!(event.exit_code, 0);
        assert!(event.notify, "NotifyPolicy::Always must always notify");
    }

    #[tokio::test]
    async fn reports_a_failing_task() {
        let event = run_once("exit 2").await;
        assert_eq!(event.exit_code, 2);
        assert!(event.notice().contains("Exit 2"));
    }

    /// The running flag has to be cleared after a run, or the task would never
    /// fire a second time.
    #[tokio::test]
    async fn a_finished_task_is_not_left_marked_running() {
        let scheduler = SchedulerState::shared();
        {
            let mut state = scheduler.write();
            state.add(spec("true"), HashMap::new()).unwrap();
            state.trigger("probe").unwrap();
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(scheduler_task(Arc::clone(&scheduler), tx));
        tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("runner did not report within 10s");
        handle.abort();

        let view = scheduler.read().view("probe").unwrap();
        assert!(!view.running);
        assert_eq!(view.run_count, 1);
    }

    #[tokio::test]
    async fn an_empty_scheduler_reports_nothing() {
        let scheduler = SchedulerState::shared();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(scheduler_task(scheduler, tx));

        let result = tokio::time::timeout(Duration::from_millis(2500), rx.recv()).await;
        handle.abort();
        assert!(result.is_err(), "idle scheduler produced an event");
    }
}
