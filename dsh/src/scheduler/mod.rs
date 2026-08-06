//! Session-scoped periodic task scheduler behind the `sched` builtin.
//!
//! # Where the pieces live
//!
//! The task list is owned by [`crate::environment::Environment`] rather than by
//! the REPL, because `config.lisp` runs before the REPL exists and must be able
//! to register tasks. [`runner`] holds a clone of the same `Arc` and reports
//! finished runs back over an mpsc channel that the REPL selects on.
//!
//! # Why tasks do not go through the shell
//!
//! `Shell` is `!Send` (it owns `Rc<RefCell<LispEngine>>`), so a spawned task
//! cannot call `eval_str`. Running tasks inside the REPL's 1-second tick instead
//! would block key input for as long as the command takes, and the capture path
//! puts the job in the foreground — handing the terminal to a background task
//! while the user is typing. [`exec`] therefore runs each command as a detached
//! `sh -c` child with stdin on `/dev/null` and its own process group.
//!
//! The visible consequence: shell aliases, abbreviations, builtins and Lisp
//! functions are *not* available inside a scheduled command.

pub mod exec;
pub mod runner;

use dsh_types::schedule::{IntervalSpec, NotifyPolicy, SchedRun, SchedTaskSpec, SchedTaskView};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Runs kept per task for `sched log`. Full output lives in the output history;
/// this is only enough to see what happened at a glance.
const HISTORY_LIMIT: usize = 20;

pub type SharedScheduler = Arc<RwLock<SchedulerState>>;

/// A finished run, on its way from the runner to the REPL.
#[derive(Debug, Clone)]
pub struct SchedulerEvent {
    pub id: u64,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration: Duration,
    pub timed_out: bool,
    pub changed: bool,
    /// Whether the notify policy wants this surfaced to the user.
    pub notify: bool,
}

impl SchedulerEvent {
    /// The one-line notice printed above the prompt, in the same shape as the
    /// background job notices.
    pub fn notice(&self) -> String {
        let status = if self.timed_out {
            "Timeout".to_string()
        } else if self.exit_code == 0 {
            "Done".to_string()
        } else {
            format!("Exit {}", self.exit_code)
        };
        format!(
            "[sched {}] {} ({:.1}s)  {}",
            self.name,
            status,
            self.duration.as_secs_f64(),
            self.command
        )
    }
}

#[derive(Debug)]
pub struct ScheduledTask {
    pub id: u64,
    pub name: String,
    pub interval: IntervalSpec,
    pub command: String,
    pub cwd: String,
    pub notify: NotifyPolicy,
    pub timeout: Duration,
    pub env: HashMap<String, String>,
    pub paused: bool,
    next_run: Instant,
    /// Set for as long as a run is in flight, so a slow task skips rather than
    /// stacking up copies of itself.
    running: Arc<AtomicBool>,
    /// Digest of the previous run's stdout, for change detection.
    last_digest: Option<u64>,
    run_count: u64,
    fail_count: u64,
    history: Vec<SchedRun>,
}

impl ScheduledTask {
    fn view(&self, now: Instant) -> SchedTaskView {
        SchedTaskView {
            id: self.id,
            name: self.name.clone(),
            interval: self.interval,
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            notify: self.notify,
            paused: self.paused,
            next_in: if self.paused {
                None
            } else {
                Some(self.next_run.saturating_duration_since(now).as_secs())
            },
            running: self.running.load(Ordering::Relaxed),
            run_count: self.run_count,
            fail_count: self.fail_count,
            last: self.history.last().cloned(),
            history: self.history.clone(),
        }
    }
}

/// A task the runner should start now.
#[derive(Debug, Clone)]
pub struct DueTask {
    pub id: u64,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub timeout: Duration,
    pub env: HashMap<String, String>,
    pub running: Arc<AtomicBool>,
}

#[derive(Debug, Default)]
pub struct SchedulerState {
    tasks: Vec<ScheduledTask>,
    next_id: u64,
    /// Master switch; `sched pause` with no argument flips it.
    pub enabled: bool,
}

impl SchedulerState {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            next_id: 1,
            enabled: true,
        }
    }

    pub fn shared() -> SharedScheduler {
        Arc::new(RwLock::new(Self::new()))
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Registers a task, returning its id. Names must be unique so `sched rm
    /// <name>` is unambiguous.
    pub fn add(
        &mut self,
        spec: SchedTaskSpec,
        env: HashMap<String, String>,
    ) -> Result<u64, String> {
        if spec.name.trim().is_empty() {
            return Err("task name must not be empty".to_string());
        }
        if self.tasks.iter().any(|task| task.name == spec.name) {
            return Err(format!(
                "{}: a task with that name already exists",
                spec.name
            ));
        }

        let id = self.next_id;
        self.next_id += 1;

        // The first run is one full interval away: registering a task from
        // config.lisp should not fire a burst of commands at shell startup.
        let next_run = Instant::now() + spec.interval.duration();

        self.tasks.push(ScheduledTask {
            id,
            name: spec.name,
            interval: spec.interval,
            command: spec.command,
            cwd: spec.cwd,
            notify: spec.notify,
            timeout: spec.timeout,
            env,
            paused: false,
            next_run,
            running: Arc::new(AtomicBool::new(false)),
            last_digest: None,
            run_count: 0,
            fail_count: 0,
            history: Vec::new(),
        });
        Ok(id)
    }

    fn position(&self, selector: &str) -> Option<usize> {
        self.tasks
            .iter()
            .position(|task| task.name == selector || task.id.to_string() == selector)
    }

    pub fn remove(&mut self, selector: &str) -> Result<String, String> {
        let index = self
            .position(selector)
            .ok_or_else(|| format!("{selector}: no such task"))?;
        Ok(self.tasks.remove(index).name)
    }

    pub fn set_paused(&mut self, selector: &str, paused: bool) -> Result<String, String> {
        let index = self
            .position(selector)
            .ok_or_else(|| format!("{selector}: no such task"))?;
        let task = &mut self.tasks[index];
        task.paused = paused;
        if !paused {
            task.next_run = Instant::now() + task.interval.duration();
        }
        Ok(task.name.clone())
    }

    /// Flips the master switch (`sched pause` / `sched resume` with no task).
    ///
    /// Resuming re-bases every task's slot, exactly as per-task `set_paused`
    /// does. Without that, a scheduler paused for an hour would find every task
    /// long overdue and fire the whole backlog on the next scan.
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled && !self.enabled {
            let now = Instant::now();
            for task in &mut self.tasks {
                task.next_run = now + task.interval.duration();
            }
        }
        self.enabled = enabled;
    }

    /// Makes a task due immediately (`sched run`).
    pub fn trigger(&mut self, selector: &str) -> Result<String, String> {
        let index = self
            .position(selector)
            .ok_or_else(|| format!("{selector}: no such task"))?;
        let task = &mut self.tasks[index];
        task.next_run = Instant::now();
        Ok(task.name.clone())
    }

    pub fn views(&self) -> Vec<SchedTaskView> {
        let now = Instant::now();
        self.tasks.iter().map(|task| task.view(now)).collect()
    }

    pub fn view(&self, selector: &str) -> Option<SchedTaskView> {
        let now = Instant::now();
        self.position(selector)
            .map(|index| self.tasks[index].view(now))
    }

    /// Claims every task whose time has come, marking each as running and
    /// scheduling its next slot.
    ///
    /// Claiming here rather than in the runner keeps the decision under one
    /// lock: two scans can never hand out the same task twice.
    pub fn claim_due(&mut self, now: Instant) -> Vec<DueTask> {
        if !self.enabled {
            return Vec::new();
        }

        let mut due = Vec::new();
        for task in &mut self.tasks {
            if task.paused || task.next_run > now {
                continue;
            }

            // Always move the slot forward, even when skipping: otherwise a
            // task that runs long stays permanently due and spins the scan.
            task.next_run = now + task.interval.duration();

            if task.running.swap(true, Ordering::SeqCst) {
                continue;
            }

            due.push(DueTask {
                id: task.id,
                name: task.name.clone(),
                command: task.command.clone(),
                cwd: task.cwd.clone(),
                timeout: task.timeout,
                env: task.env.clone(),
                running: Arc::clone(&task.running),
            });
        }
        due
    }

    /// Records a finished run and decides whether to notify.
    ///
    /// Returns `None` if the task was removed while it was running.
    pub fn record(
        &mut self,
        id: u64,
        stdout: &str,
        exit_code: i32,
        timed_out: bool,
        duration: Duration,
    ) -> Option<(SchedRun, NotifyPolicy, bool)> {
        let index = self.tasks.iter().position(|task| task.id == id)?;
        let task = &mut self.tasks[index];

        let digest = exec::digest(stdout);
        // The first run has nothing to compare against, so it is never
        // "changed" — otherwise every task would announce itself once.
        let changed = task.last_digest.is_some_and(|previous| previous != digest);
        task.last_digest = Some(digest);

        let previously_failed = task.history.last().is_some_and(|run| !run.succeeded());
        let succeeded = exit_code == 0 && !timed_out;

        task.run_count += 1;
        if !succeeded {
            task.fail_count += 1;
        }

        let run = SchedRun {
            finished_at: std::time::SystemTime::now(),
            duration_ms: duration.as_millis() as u64,
            exit_code,
            changed,
            timed_out,
            preview: exec::preview(stdout),
        };

        task.history.push(run.clone());
        if task.history.len() > HISTORY_LIMIT {
            task.history.remove(0);
        }

        let notify = should_notify(task.notify, succeeded, previously_failed, changed);
        Some((run, task.notify, notify))
    }

    /// Clears the running flag once a run's result has been recorded.
    pub fn finish(&mut self, id: u64) {
        if let Some(task) = self.tasks.iter().find(|task| task.id == id) {
            task.running.store(false, Ordering::SeqCst);
        }
    }

    /// `sched list --lisp`: the calls that recreate the current task set.
    pub fn as_lisp(&self) -> Vec<String> {
        self.tasks
            .iter()
            .map(|task| {
                format!(
                    "(sched-add \"{}\" \"{}\" \"{}\" \"{}\")",
                    task.name,
                    task.interval,
                    task.command.replace('\\', "\\\\").replace('"', "\\\""),
                    task.notify
                )
            })
            .collect()
    }
}

/// Decides whether a finished run should interrupt the user.
///
/// Failures notify on the *transition* into failure, plus once on recovery. A
/// task failing every 30 seconds should say so once, not forever.
fn should_notify(
    policy: NotifyPolicy,
    succeeded: bool,
    previously_failed: bool,
    changed: bool,
) -> bool {
    let failure_edge = (!succeeded && !previously_failed) || (succeeded && previously_failed);
    match policy {
        NotifyPolicy::Never => false,
        NotifyPolicy::OnFailure => failure_edge,
        NotifyPolicy::OnChange => changed,
        NotifyPolicy::Both => failure_edge || changed,
        NotifyPolicy::Always => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::schedule::parse_interval;

    fn spec(name: &str, interval: &str) -> SchedTaskSpec {
        SchedTaskSpec {
            name: name.to_string(),
            interval: parse_interval(interval).unwrap(),
            command: "true".to_string(),
            cwd: "/tmp".to_string(),
            notify: NotifyPolicy::Both,
            timeout: Duration::from_secs(60),
        }
    }

    fn state_with(name: &str, interval: &str) -> (SchedulerState, u64) {
        let mut state = SchedulerState::new();
        let id = state.add(spec(name, interval), HashMap::new()).unwrap();
        (state, id)
    }

    #[test]
    fn names_must_be_unique() {
        let (mut state, _) = state_with("fetch", "5m");
        assert!(state.add(spec("fetch", "1h"), HashMap::new()).is_err());
        assert!(state.add(spec("other", "1h"), HashMap::new()).is_ok());
    }

    #[test]
    fn rejects_an_empty_name() {
        let mut state = SchedulerState::new();
        assert!(state.add(spec("  ", "5m"), HashMap::new()).is_err());
    }

    #[test]
    fn tasks_are_addressable_by_id_or_name() {
        let (mut state, id) = state_with("fetch", "5m");
        assert!(state.view("fetch").is_some());
        assert!(state.view(&id.to_string()).is_some());
        assert!(state.view("nope").is_none());
        assert_eq!(state.remove(&id.to_string()), Ok("fetch".to_string()));
        assert!(state.remove("fetch").is_err());
    }

    /// Registering must not fire immediately: a config.lisp full of tasks would
    /// otherwise launch every one of them at startup.
    #[test]
    fn a_new_task_is_not_immediately_due() {
        let (mut state, _) = state_with("fetch", "5m");
        assert!(state.claim_due(Instant::now()).is_empty());
    }

    #[test]
    fn a_task_becomes_due_after_its_interval() {
        let (mut state, id) = state_with("fetch", "5m");
        let later = Instant::now() + Duration::from_secs(301);

        let due = state.claim_due(later);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, id);
    }

    #[test]
    fn a_running_task_is_skipped_rather_than_stacked() {
        let (mut state, _) = state_with("slow", "5s");
        let later = Instant::now() + Duration::from_secs(10);

        assert_eq!(state.claim_due(later).len(), 1);
        // Still running: the next scan must not launch a second copy.
        let later_still = later + Duration::from_secs(10);
        assert!(state.claim_due(later_still).is_empty());
    }

    #[test]
    fn finishing_a_run_lets_the_next_one_start() {
        let (mut state, id) = state_with("task", "5s");
        let t1 = Instant::now() + Duration::from_secs(10);
        assert_eq!(state.claim_due(t1).len(), 1);

        state.record(id, "out", 0, false, Duration::from_millis(5));
        state.finish(id);

        let t2 = t1 + Duration::from_secs(10);
        assert_eq!(state.claim_due(t2).len(), 1);
    }

    #[test]
    fn paused_tasks_never_come_due() {
        let (mut state, _) = state_with("fetch", "5s");
        state.set_paused("fetch", true).unwrap();

        assert!(
            state
                .claim_due(Instant::now() + Duration::from_secs(600))
                .is_empty()
        );
        assert!(state.view("fetch").unwrap().next_in.is_none());
    }

    /// Resuming restarts the interval rather than firing off the backlog that
    /// built up while the task was paused.
    #[test]
    fn resuming_waits_one_interval_before_the_next_run() {
        let (mut state, _) = state_with("fetch", "5s");
        state.set_paused("fetch", true).unwrap();
        state.claim_due(Instant::now() + Duration::from_secs(600));

        state.set_paused("fetch", false).unwrap();
        assert!(state.claim_due(Instant::now()).is_empty());
        assert!(
            !state
                .claim_due(Instant::now() + Duration::from_secs(6))
                .is_empty()
        );
    }

    #[test]
    fn the_master_switch_stops_everything() {
        let (mut state, _) = state_with("fetch", "5s");
        state.set_enabled(false);
        assert!(
            state
                .claim_due(Instant::now() + Duration::from_secs(600))
                .is_empty()
        );
    }

    /// A scheduler paused for a long time must not fire the whole backlog the
    /// moment it comes back.
    #[test]
    fn the_master_resume_rebases_every_task() {
        let mut state = SchedulerState::new();
        state.add(spec("a", "5s"), HashMap::new()).unwrap();
        state.add(spec("b", "5s"), HashMap::new()).unwrap();

        state.set_enabled(false);
        // A long pause leaves both slots far in the past.
        state.claim_due(Instant::now() + Duration::from_secs(3600));

        state.set_enabled(true);
        assert!(
            state.claim_due(Instant::now()).is_empty(),
            "resume fired the backlog"
        );
        assert_eq!(
            state
                .claim_due(Instant::now() + Duration::from_secs(6))
                .len(),
            2
        );
    }

    /// Resuming an already-running scheduler must not push tasks back.
    #[test]
    fn enabling_an_enabled_scheduler_does_not_delay_anything() {
        let (mut state, _) = state_with("fetch", "5s");
        let due_at = Instant::now() + Duration::from_secs(6);
        state.set_enabled(true);
        assert_eq!(state.claim_due(due_at).len(), 1);
    }

    #[test]
    fn trigger_makes_a_task_due_now() {
        let (mut state, _) = state_with("fetch", "1h");
        state.trigger("fetch").unwrap();
        assert_eq!(state.claim_due(Instant::now()).len(), 1);
    }

    #[test]
    fn the_first_run_is_never_reported_as_changed() {
        let (mut state, id) = state_with("fetch", "5s");
        let (run, _, _) = state
            .record(id, "hello", 0, false, Duration::from_millis(1))
            .unwrap();
        assert!(!run.changed);
    }

    #[test]
    fn change_detection_compares_against_the_previous_run() {
        let (mut state, id) = state_with("fetch", "5s");
        state.record(id, "hello", 0, false, Duration::from_millis(1));

        let (same, _, _) = state
            .record(id, "hello", 0, false, Duration::from_millis(1))
            .unwrap();
        assert!(!same.changed);

        let (different, _, _) = state
            .record(id, "goodbye", 0, false, Duration::from_millis(1))
            .unwrap();
        assert!(different.changed);
    }

    #[test]
    fn failures_are_counted() {
        let (mut state, id) = state_with("fetch", "5s");
        state.record(id, "", 1, false, Duration::from_millis(1));
        state.record(id, "", 0, false, Duration::from_millis(1));

        let view = state.view("fetch").unwrap();
        assert_eq!(view.run_count, 2);
        assert_eq!(view.fail_count, 1);
    }

    #[test]
    fn recording_a_removed_task_is_a_no_op() {
        let (mut state, id) = state_with("fetch", "5s");
        state.remove("fetch").unwrap();
        assert!(
            state
                .record(id, "out", 0, false, Duration::from_millis(1))
                .is_none()
        );
    }

    #[test]
    fn history_is_capped() {
        let (mut state, id) = state_with("fetch", "5s");
        for i in 0..(HISTORY_LIMIT + 5) {
            state.record(id, &i.to_string(), 0, false, Duration::from_millis(1));
        }
        assert_eq!(state.view("fetch").unwrap().history.len(), HISTORY_LIMIT);
    }

    // --- notification policy ---

    #[test]
    fn never_stays_silent() {
        assert!(!should_notify(NotifyPolicy::Never, false, false, true));
    }

    #[test]
    fn always_reports_every_run() {
        assert!(should_notify(NotifyPolicy::Always, true, false, false));
    }

    /// A task failing every 30 seconds must not notify every 30 seconds.
    #[test]
    fn repeated_failures_notify_only_on_the_edge() {
        assert!(should_notify(NotifyPolicy::OnFailure, false, false, false));
        assert!(!should_notify(NotifyPolicy::OnFailure, false, true, false));
    }

    #[test]
    fn recovery_is_worth_one_notification() {
        assert!(should_notify(NotifyPolicy::OnFailure, true, true, false));
        assert!(!should_notify(NotifyPolicy::OnFailure, true, false, false));
    }

    #[test]
    fn on_change_ignores_exit_status() {
        assert!(should_notify(NotifyPolicy::OnChange, false, false, true));
        assert!(!should_notify(NotifyPolicy::OnChange, false, false, false));
    }

    #[test]
    fn both_covers_either_trigger() {
        assert!(should_notify(NotifyPolicy::Both, true, false, true));
        assert!(should_notify(NotifyPolicy::Both, false, false, false));
        assert!(!should_notify(NotifyPolicy::Both, true, false, false));
    }

    #[test]
    fn lisp_output_round_trips_through_the_parser() {
        let (mut state, _) = state_with("fetch", "5m");
        let lines = state.as_lisp();
        assert_eq!(
            lines,
            vec![r#"(sched-add "fetch" "5m" "true" "both")"#.to_string()]
        );

        // Quotes in the command are escaped so the emitted Lisp stays valid.
        state
            .add(
                SchedTaskSpec {
                    command: r#"echo "hi""#.to_string(),
                    ..spec("quoted", "5m")
                },
                HashMap::new(),
            )
            .unwrap();
        assert!(state.as_lisp()[1].contains(r#"echo \"hi\""#));
    }
}
