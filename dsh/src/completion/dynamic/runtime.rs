use super::worker::{CompletionWorkerPool, Job};
use parking_lot::RwLock;
use std::sync::OnceLock;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Copy)]
pub(crate) struct CompletionWorkerCounts {
    command: usize,
    external: usize,
    fish: usize,
}

impl CompletionWorkerCounts {
    #[cfg(test)]
    pub(crate) const fn new(command: usize, external: usize, fish: usize) -> Self {
        Self {
            command,
            external,
            fish,
        }
    }
}

impl Default for CompletionWorkerCounts {
    fn default() -> Self {
        let command = std::thread::available_parallelism()
            .map(|count| count.get().clamp(2, 8))
            .unwrap_or(4);
        Self {
            command,
            external: 4,
            fish: 4,
        }
    }
}

pub(crate) struct CompletionRuntime {
    worker_counts: CompletionWorkerCounts,
    command_workers: OnceLock<CompletionWorkerPool>,
    external_workers: OnceLock<CompletionWorkerPool>,
    fish_workers: OnceLock<CompletionWorkerPool>,
    notifier: RwLock<Option<UnboundedSender<()>>>,
    pub(super) diagnostics: RwLock<super::DynamicCompletionDiagnostics>,
}

impl CompletionRuntime {
    pub(crate) fn new() -> Self {
        Self::with_worker_counts(CompletionWorkerCounts::default())
    }

    pub(crate) fn with_worker_counts(worker_counts: CompletionWorkerCounts) -> Self {
        Self {
            worker_counts,
            command_workers: OnceLock::new(),
            external_workers: OnceLock::new(),
            fish_workers: OnceLock::new(),
            notifier: RwLock::new(None),
            diagnostics: RwLock::new(super::DynamicCompletionDiagnostics::default()),
        }
    }

    pub(crate) fn set_notifier(&self, sender: UnboundedSender<()>) {
        *self.notifier.write() = Some(sender);
    }

    pub(crate) fn notify(&self) {
        if let Some(sender) = self.notifier.read().as_ref() {
            let _ = sender.send(());
        }
    }

    pub(crate) fn diagnostics_lines(&self) -> Vec<String> {
        super::diagnostics_lines(self)
    }

    pub(super) fn submit_command(&self, job: Job) -> bool {
        self.command_workers
            .get_or_init(|| CompletionWorkerPool::new("dynamic", self.worker_counts.command))
            .try_submit(job)
    }

    pub(super) fn submit_external(&self, is_fish: bool, job: Job) -> bool {
        if is_fish {
            self.fish_workers
                .get_or_init(|| CompletionWorkerPool::new("fish", self.worker_counts.fish))
                .try_submit(job)
        } else {
            self.external_workers
                .get_or_init(|| CompletionWorkerPool::new("external", self.worker_counts.external))
                .try_submit(job)
        }
    }

    pub(super) fn record_queue_drop(&self, kind: &'static str) {
        let mut diagnostics = self.diagnostics.write();
        diagnostics.queue_dropped_total += 1;
        diagnostics.last_external = Some(format!("{kind} refresh dropped: queue full"));
    }
}

impl Default for CompletionRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    #[test]
    fn notifier_is_scoped_to_each_runtime() {
        let first = CompletionRuntime::new();
        let second = CompletionRuntime::new();
        let (first_tx, mut first_rx) = tokio::sync::mpsc::unbounded_channel();
        let (second_tx, mut second_rx) = tokio::sync::mpsc::unbounded_channel();
        first.set_notifier(first_tx);
        second.set_notifier(second_tx);

        first.notify();

        assert_eq!(first_rx.try_recv(), Ok(()));
        assert!(second_rx.try_recv().is_err());
    }

    #[test]
    fn full_queue_is_rejected_without_waiting() {
        let runtime = Arc::new(CompletionRuntime::with_worker_counts(
            CompletionWorkerCounts::new(1, 1, 1),
        ));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, started_rx) = mpsc::channel::<()>();
        assert!(runtime.submit_command(Box::new(move || {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
        })));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        for _ in 0..4 {
            assert!(runtime.submit_command(Box::new(|| {})));
        }
        assert!(!runtime.submit_command(Box::new(|| {})));
        release_tx.send(()).unwrap();
    }
}
