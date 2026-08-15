use crate::command_timing::{
    CommandTiming, CommandTimingSnapshot, SharedCommandTiming, TIMING_SAVE_RECORD_THRESHOLD,
    TimingWriteOutcome,
};
use crate::history::{
    CommandHistoryReloadSnapshot, FrecencyHistory, FrecencyReloadApply, FrecencyReloadSnapshot,
    History, HistoryReloadApply,
};
use parking_lot::{Mutex as ParkingMutex, RwLock};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, warn};

#[derive(Debug)]
pub(crate) enum BackgroundIoEvent {
    CommandHistory(Result<CommandHistoryReloadSnapshot, String>),
    Frecency(Result<FrecencyReloadSnapshot, String>),
    TimingSaved {
        generation: u64,
        dirty_records: u64,
        result: Result<TimingWriteOutcome, String>,
    },
}

struct TimingWriteJob {
    path: PathBuf,
    snapshot: CommandTimingSnapshot,
}

pub(crate) struct BackgroundIoCoordinator {
    result_tx: UnboundedSender<BackgroundIoEvent>,
    timing_tx: Option<SyncSender<TimingWriteJob>>,
    timing_worker: Option<JoinHandle<()>>,
    command_history_inflight: bool,
    frecency_inflight: bool,
    timing_inflight: bool,
}

impl BackgroundIoCoordinator {
    pub(crate) fn new(result_tx: UnboundedSender<BackgroundIoEvent>) -> Self {
        Self::with_timing_writer(result_tx, |snapshot, path| snapshot.write_to_file(path))
    }

    fn with_timing_writer<F>(result_tx: UnboundedSender<BackgroundIoEvent>, writer: F) -> Self
    where
        F: Fn(&CommandTimingSnapshot, &PathBuf) -> std::io::Result<TimingWriteOutcome>
            + Send
            + 'static,
    {
        let (timing_tx, timing_rx) = mpsc::sync_channel::<TimingWriteJob>(1);
        let worker_result_tx = result_tx.clone();
        let timing_worker = std::thread::Builder::new()
            .name("dsh-timing-writer".to_string())
            .spawn(move || {
                while let Ok(job) = timing_rx.recv() {
                    let generation = job.snapshot.generation();
                    let dirty_records = job.snapshot.dirty_records();
                    let result =
                        writer(&job.snapshot, &job.path).map_err(|error| error.to_string());
                    let _ = worker_result_tx.send(BackgroundIoEvent::TimingSaved {
                        generation,
                        dirty_records,
                        result,
                    });
                }
            })
            .expect("command timing writer thread must start");

        Self {
            result_tx,
            timing_tx: Some(timing_tx),
            timing_worker: Some(timing_worker),
            command_history_inflight: false,
            frecency_inflight: false,
            timing_inflight: false,
        }
    }

    pub(crate) fn schedule_history_sync(
        &mut self,
        command_history: Option<&Arc<ParkingMutex<History>>>,
        path_history: Option<&Arc<ParkingMutex<FrecencyHistory>>>,
    ) {
        if !self.command_history_inflight
            && let Some(history) = command_history
            && let Some(mut history) = history.try_lock()
        {
            let result_tx = self.result_tx.clone();
            if history.request_reload(move |result| {
                let _ = result_tx.send(BackgroundIoEvent::CommandHistory(
                    result.map_err(|error| error.to_string()),
                ));
            }) {
                self.command_history_inflight = true;
            }
        }

        if !self.frecency_inflight
            && let Some(history) = path_history
            && let Some(history) = history.try_lock()
        {
            let result_tx = self.result_tx.clone();
            if history.request_reload(move |result| {
                let _ = result_tx.send(BackgroundIoEvent::Frecency(
                    result.map_err(|error| error.to_string()),
                ));
            }) {
                self.frecency_inflight = true;
            }
        }
    }

    pub(crate) fn schedule_timing_save(&mut self, timing: &SharedCommandTiming, path: PathBuf) {
        let mut timing = timing.write();
        timing.reconcile_configured_storage();
        if self.timing_inflight {
            return;
        }
        let Some(snapshot) = timing.background_snapshot_if_due() else {
            return;
        };
        let Some(sender) = &self.timing_tx else {
            return;
        };

        match sender.try_send(TimingWriteJob { path, snapshot }) {
            Ok(()) => self.timing_inflight = true,
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {
                warn!("command timing writer is unavailable");
            }
        }
    }

    pub(crate) fn apply_event(
        &mut self,
        event: BackgroundIoEvent,
        command_history: Option<&Arc<ParkingMutex<History>>>,
        path_history: Option<&Arc<ParkingMutex<FrecencyHistory>>>,
        timing: &SharedCommandTiming,
    ) {
        match event {
            BackgroundIoEvent::CommandHistory(result) => {
                self.command_history_inflight = false;
                match result {
                    Ok(snapshot) => {
                        let Some(history) = command_history.and_then(|history| history.try_lock())
                        else {
                            debug!("discarding command history reload while history is busy");
                            return;
                        };
                        let mut history = history;
                        match history.apply_reload_snapshot(snapshot) {
                            HistoryReloadApply::Applied => {}
                            HistoryReloadApply::Navigating => {
                                debug!("discarding command history reload during navigation");
                            }
                            HistoryReloadApply::Stale => {
                                debug!("discarding stale command history reload");
                            }
                        }
                    }
                    Err(error) => warn!("background command history reload failed: {error}"),
                }
            }
            BackgroundIoEvent::Frecency(result) => {
                self.frecency_inflight = false;
                match result {
                    Ok(snapshot) => {
                        let Some(history) = path_history.and_then(|history| history.try_lock())
                        else {
                            debug!("discarding frecency reload while history is busy");
                            return;
                        };
                        let mut history = history;
                        if history.apply_reload_snapshot(snapshot) == FrecencyReloadApply::Stale {
                            debug!("discarding stale frecency reload");
                        }
                    }
                    Err(error) => warn!("background frecency reload failed: {error}"),
                }
            }
            BackgroundIoEvent::TimingSaved {
                generation,
                dirty_records,
                result,
            } => {
                self.timing_inflight = false;
                match result {
                    Ok(TimingWriteOutcome::Written) => timing
                        .write()
                        .acknowledge_background_save(generation, dirty_records),
                    Ok(TimingWriteOutcome::SupersededByReset) => {
                        debug!("discarding command timing snapshot superseded by reset");
                    }
                    Err(error) => warn!("background command timing save failed: {error}"),
                }
            }
        }
    }

    pub(crate) fn shutdown(&mut self) {
        self.timing_tx.take();
        if let Some(worker) = self.timing_worker.take()
            && worker.join().is_err()
        {
            warn!("command timing writer thread panicked");
        }
        self.timing_inflight = false;
    }
}

impl Drop for BackgroundIoCoordinator {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) fn probe_schedule_cost() -> Duration {
    let (result_tx, _result_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut coordinator =
        BackgroundIoCoordinator::with_timing_writer(result_tx, |_snapshot, _path| {
            Ok(TimingWriteOutcome::Written)
        });
    let timing = Arc::new(RwLock::new(CommandTiming::new()));
    for _ in 0..TIMING_SAVE_RECORD_THRESHOLD {
        timing.write().record("probe", 0, Duration::from_millis(1));
    }

    let start = Instant::now();
    coordinator.schedule_timing_save(&timing, PathBuf::from("unused.json"));
    let elapsed = start.elapsed();
    coordinator.shutdown();
    elapsed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_timing::CommandTiming;
    use std::sync::{Arc, Barrier};

    #[test]
    fn timing_write_runs_off_the_scheduling_thread() {
        let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel();
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_started = Arc::clone(&started);
        let worker_release = Arc::clone(&release);
        let mut coordinator =
            BackgroundIoCoordinator::with_timing_writer(result_tx, move |_snapshot, _path| {
                worker_started.wait();
                worker_release.wait();
                Ok(TimingWriteOutcome::Written)
            });
        let timing = Arc::new(RwLock::new(CommandTiming::new()));
        for _ in 0..crate::command_timing::TIMING_SAVE_RECORD_THRESHOLD {
            timing.write().record("git", 0, Duration::from_millis(1));
        }

        coordinator.schedule_timing_save(&timing, PathBuf::from("unused.json"));
        started.wait();
        assert!(coordinator.timing_inflight);

        // A duplicate tick cannot queue another write while this one is held.
        coordinator.schedule_timing_save(&timing, PathBuf::from("unused.json"));

        timing.write().record("cargo", 0, Duration::from_millis(2));
        release.wait();
        let event = result_rx.blocking_recv().expect("timing result");
        coordinator.apply_event(event, None, None, &timing);
        assert!(!coordinator.timing_inflight);
        assert!(timing.read().is_dirty());
        assert!(result_rx.try_recv().is_err());
    }

    #[test]
    fn reset_is_applied_even_while_an_older_timing_write_is_inflight() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timing.json");
        let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel();
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_started = Arc::clone(&started);
        let worker_release = Arc::clone(&release);
        let mut coordinator =
            BackgroundIoCoordinator::with_timing_writer(result_tx, move |_snapshot, _path| {
                worker_started.wait();
                worker_release.wait();
                Ok(TimingWriteOutcome::Written)
            });
        let timing = Arc::new(RwLock::new(CommandTiming::new_for_path(path.clone())));
        for _ in 0..TIMING_SAVE_RECORD_THRESHOLD {
            timing.write().record("git", 0, Duration::from_millis(1));
        }
        coordinator.schedule_timing_save(&timing, path.clone());
        started.wait();

        dsh_builtin::command_timing::CommandTiming::new()
            .save_to_file(&path)
            .unwrap();
        timing.write().record("timing", 0, Duration::from_millis(1));
        coordinator.schedule_timing_save(&timing, path);

        assert!(timing.read().stats.is_empty());
        assert!(!timing.read().is_dirty());

        release.wait();
        let event = result_rx.blocking_recv().expect("timing result");
        coordinator.apply_event(event, None, None, &timing);
    }

    #[tokio::test]
    async fn resize_is_processed_while_timing_writer_is_blocked() {
        let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel();
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_started = Arc::clone(&started);
        let worker_release = Arc::clone(&release);
        let mut coordinator =
            BackgroundIoCoordinator::with_timing_writer(result_tx, move |_snapshot, _path| {
                worker_started.wait();
                worker_release.wait();
                Ok(TimingWriteOutcome::Written)
            });
        let timing = Arc::new(RwLock::new(CommandTiming::new()));
        for _ in 0..TIMING_SAVE_RECORD_THRESHOLD {
            timing.write().record("git", 0, Duration::from_millis(1));
        }
        coordinator.schedule_timing_save(&timing, PathBuf::from("unused.json"));
        started.wait();

        let environment = crate::environment::Environment::new();
        let mut shell = crate::shell::Shell::new(environment);
        let mut repl = super::super::Repl::new(&mut shell);
        repl.terminal_ui.columns = 80;
        repl.terminal_ui.lines = 24;

        let result = repl
            .handle_event(crate::repl::state::ShellEvent::Input(
                crossterm::event::Event::Resize(120, 40),
            ))
            .await
            .unwrap();

        assert!(matches!(
            result,
            crate::repl::state::ReplControlFlow::Continue
        ));
        assert_eq!(repl.terminal_ui.columns, 120);
        assert_eq!(repl.terminal_ui.lines, 40);

        release.wait();
        let event = result_rx.recv().await.expect("timing result");
        coordinator.apply_event(event, None, None, &timing);
    }

    #[test]
    fn failed_timing_write_clears_inflight_but_keeps_data_dirty() {
        let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut coordinator =
            BackgroundIoCoordinator::with_timing_writer(result_tx, |_snapshot, _path| {
                Err(std::io::Error::other("disk unavailable"))
            });
        let timing = Arc::new(RwLock::new(CommandTiming::new()));
        for _ in 0..crate::command_timing::TIMING_SAVE_RECORD_THRESHOLD {
            timing.write().record("git", 0, Duration::from_millis(1));
        }

        coordinator.schedule_timing_save(&timing, PathBuf::from("unused.json"));
        let event = result_rx.blocking_recv().expect("timing result");
        coordinator.apply_event(event, None, None, &timing);

        assert!(!coordinator.timing_inflight);
        assert!(timing.read().is_dirty());
    }

    #[test]
    fn failed_history_reload_clears_inflight_and_keeps_current_state() {
        let (result_tx, _result_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut coordinator = BackgroundIoCoordinator::new(result_tx);
        coordinator.command_history_inflight = true;
        let mut history = History::new();
        history.add_test_entry("keep me");
        let history = Arc::new(ParkingMutex::new(history));
        let timing = Arc::new(RwLock::new(CommandTiming::new()));

        coordinator.apply_event(
            BackgroundIoEvent::CommandHistory(Err("database unavailable".to_string())),
            Some(&history),
            None,
            &timing,
        );

        assert!(!coordinator.command_history_inflight);
        assert_eq!(history.lock().iter().next().unwrap().entry, "keep me");
    }

    #[test]
    fn history_reload_reservation_does_not_wait_for_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::new(dir.path().join("history.db")).unwrap();
        let blocker_db = db.clone();
        let connection_guard = blocker_db.get_connection();
        let mut history = History::new();
        history.db = Some(db);
        history.start_background_writer();
        let history = Arc::new(ParkingMutex::new(history));
        let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut coordinator = BackgroundIoCoordinator::new(result_tx);

        coordinator.schedule_history_sync(Some(&history), None);
        coordinator.schedule_history_sync(Some(&history), None);

        assert!(coordinator.command_history_inflight);
        assert!(result_rx.try_recv().is_err());

        drop(connection_guard);
        let event = result_rx.blocking_recv().expect("history result");
        coordinator.apply_event(
            event,
            Some(&history),
            None,
            &Arc::new(RwLock::new(CommandTiming::new())),
        );

        assert!(!coordinator.command_history_inflight);
        assert!(result_rx.try_recv().is_err());
    }

    #[test]
    fn shutdown_waits_for_timing_writer_before_final_flush() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timing.json");
        let (result_tx, _result_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut coordinator = BackgroundIoCoordinator::new(result_tx);
        let timing = Arc::new(RwLock::new(CommandTiming::new()));
        for _ in 0..crate::command_timing::TIMING_SAVE_RECORD_THRESHOLD {
            timing.write().record("git", 0, Duration::from_millis(1));
        }

        coordinator.schedule_timing_save(&timing, path.clone());
        coordinator.shutdown();

        assert!(path.exists());
        timing.write().save_to_file(&path).unwrap();
        assert!(!timing.read().is_dirty());
    }
}
