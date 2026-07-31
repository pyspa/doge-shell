use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, mpsc};

pub(super) type Job = Box<dyn FnOnce() + Send + 'static>;

pub(super) struct CompletionWorkerPool {
    sender: mpsc::SyncSender<Job>,
}

impl CompletionWorkerPool {
    pub(super) fn new(name: &'static str, worker_count: usize) -> Self {
        let worker_count = worker_count.max(1);
        let (sender, receiver) = mpsc::sync_channel::<Job>(worker_count * 4);
        let receiver = Arc::new(Mutex::new(receiver));

        for index in 0..worker_count {
            let receiver = Arc::clone(&receiver);
            std::thread::Builder::new()
                .name(format!("dsh-completion-{name}-{index}"))
                .spawn(move || {
                    loop {
                        let job = {
                            let receiver = receiver
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            receiver.recv()
                        };
                        let Ok(job) = job else {
                            break;
                        };
                        let _ = catch_unwind(AssertUnwindSafe(job));
                    }
                })
                .expect("completion worker thread must start");
        }

        Self { sender }
    }

    /// Never waits for capacity. A full queue is deliberately reported to the
    /// caller so TAB handling can discard refresh work and restore pending state.
    pub(super) fn try_submit(&self, job: Job) -> bool {
        self.sender.try_send(job).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn submitted_job_runs_on_named_worker() {
        let pool = CompletionWorkerPool::new("test", 1);
        let (sender, receiver) = mpsc::channel();
        assert!(pool.try_submit(Box::new(move || {
            sender
                .send(
                    std::thread::current()
                        .name()
                        .unwrap_or_default()
                        .to_string(),
                )
                .unwrap();
        })));

        let thread_name = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(thread_name.starts_with("dsh-completion-test-"));
    }
}
