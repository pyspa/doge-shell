use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, LazyLock, Mutex, mpsc};

type Job = Box<dyn FnOnce() + Send + 'static>;

struct CompletionWorkerPool {
    sender: mpsc::Sender<Job>,
}

impl CompletionWorkerPool {
    fn new(name: &'static str, worker_count: usize) -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
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

    fn submit(&self, job: Job) {
        self.sender
            .send(job)
            .expect("completion worker pool must remain available");
    }
}

#[cfg(test)]
fn command_worker_count() -> usize {
    32
}

#[cfg(not(test))]
fn command_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get().clamp(2, 8))
        .unwrap_or(4)
}

static COMMAND_WORKERS: LazyLock<CompletionWorkerPool> =
    LazyLock::new(|| CompletionWorkerPool::new("dynamic", command_worker_count()));
static EXTERNAL_WORKERS: LazyLock<CompletionWorkerPool> =
    LazyLock::new(|| CompletionWorkerPool::new("external", 4));
static FISH_WORKERS: LazyLock<CompletionWorkerPool> =
    LazyLock::new(|| CompletionWorkerPool::new("fish", 4));

pub(super) fn submit_command(job: impl FnOnce() + Send + 'static) {
    COMMAND_WORKERS.submit(Box::new(job));
}

pub(super) fn submit_external(is_fish: bool, job: impl FnOnce() + Send + 'static) {
    if is_fish {
        FISH_WORKERS.submit(Box::new(job));
    } else {
        EXTERNAL_WORKERS.submit(Box::new(job));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn submitted_jobs_run_on_separate_named_workers() {
        let (sender, receiver) = mpsc::channel();
        submit_command(move || {
            sender
                .send(
                    std::thread::current()
                        .name()
                        .unwrap_or_default()
                        .to_string(),
                )
                .unwrap();
        });

        let thread_name = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(thread_name.starts_with("dsh-completion-dynamic-"));

        let (sender, receiver) = mpsc::channel();
        submit_external(true, move || {
            sender
                .send(
                    std::thread::current()
                        .name()
                        .unwrap_or_default()
                        .to_string(),
                )
                .unwrap();
        });

        let thread_name = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(thread_name.starts_with("dsh-completion-fish-"));
    }
}
