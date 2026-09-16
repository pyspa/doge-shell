//! Managed process groups with bounded output and incremental snapshots.
use anyhow::{Result, bail};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::Read,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 1_048_576;
#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
    total: usize,
    truncated: bool,
    eof: bool,
}
fn reader(mut pipe: impl Read + Send + 'static) -> Arc<Mutex<Output>> {
    let output = Arc::new(Mutex::new(Output::default()));
    let dest = output.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = pipe.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut out = dest.lock();
            if out.bytes.len() + n > OUTPUT_LIMIT {
                let remove = out.bytes.len() + n - OUTPUT_LIMIT;
                out.bytes.drain(..remove);
                out.truncated = true;
            }
            out.bytes.extend_from_slice(&buf[..n]);
            out.total = out.total.saturating_add(n);
        }
        dest.lock().eof = true;
    });
    output
}
struct Job {
    state: Arc<Mutex<Value>>,
    cancel: Arc<AtomicBool>,
    stdout: Arc<Mutex<Output>>,
    stderr: Arc<Mutex<Output>>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Start order, so [`AgentJobs::reap_finished`] can drop the oldest
    /// finished jobs first. `HashMap` has no order of its own and nothing
    /// else here records when a job ran.
    seq: u64,
}

/// Bytes exactly as the job produced them.
///
/// The counterpart to [`AgentJobs::snapshot`], which is the model-facing
/// view: masked, and cut from the head of the requested window.
pub struct RawRead {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Absolute stream positions to resume from, the same numbers `snapshot`
    /// reports as `*_next_offset`.
    pub stdout_next: usize,
    pub stderr_next: usize,
    pub stdout_complete: bool,
    pub stderr_complete: bool,
}

#[derive(Default)]
pub struct AgentJobs {
    jobs: HashMap<String, Job>,
    next_seq: u64,
}
impl AgentJobs {
    pub fn has_running(&self) -> bool {
        self.jobs
            .values()
            .any(|job| job.state.lock()["status"] == "running")
    }

    pub fn start(
        &mut self,
        mut command: Command,
        timeout: Duration,
        config: Option<tempfile::NamedTempFile>,
    ) -> Result<String> {
        if self.jobs.len() >= 32 {
            bail!("task job limit reached (32)");
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn()?;
        let stdout = reader(child.stdout.take().expect("piped stdout"));
        let stderr = reader(child.stderr.take().expect("piped stderr"));
        let state = Arc::new(Mutex::new(json!({"status":"running", "pid":child.id()})));
        let cancel = Arc::new(AtomicBool::new(false));
        let result = state.clone();
        let stop = cancel.clone();
        let streams = [stdout.clone(), stderr.clone()];
        let worker = std::thread::spawn(move || {
            let _config = config;
            let started = Instant::now();
            let final_state = loop {
                let cancelled = stop.load(Ordering::Relaxed);
                let timed_out = started.elapsed() >= timeout;
                if cancelled || timed_out {
                    let _ = nix::sys::signal::killpg(
                        nix::unistd::Pid::from_raw(child.id() as i32),
                        nix::sys::signal::Signal::SIGKILL,
                    );
                    let _ = child.kill();
                    let _ = child.wait();
                    break json!({"status":if cancelled {"cancelled"} else {"timed_out"},"exit_code":null});
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        // A background descendant must not outlive the managed command.
                        let _ = nix::sys::signal::killpg(
                            nix::unistd::Pid::from_raw(child.id() as i32),
                            nix::sys::signal::Signal::SIGKILL,
                        );
                        break json!({"status":"exited","exit_code":status.code()});
                    }
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break json!({"status":"failed","error":error.to_string()});
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                }
            };
            let drain_deadline = Instant::now() + Duration::from_secs(2);
            while streams.iter().any(|stream| !stream.lock().eof) && Instant::now() < drain_deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            *result.lock() = final_state;
        });
        let id = uuid::Uuid::new_v4().to_string();
        let seq = self.next_seq;
        self.next_seq += 1;
        self.jobs.insert(
            id.clone(),
            Job {
                state,
                cancel,
                stdout,
                stderr,
                worker: Some(worker),
                seq,
            },
        );
        Ok(id)
    }
    pub fn snapshot(&self, id: &str, offset: usize, limit: usize) -> Result<Value> {
        let job = self.jobs.get(id).ok_or_else(|| {
            anyhow::anyhow!("unknown job (jobs from a previous process cannot be reattached)")
        })?;
        let mut value = job.state.lock().clone();
        value["job_id"] = json!(id);
        for (name, stream) in [("stdout", &job.stdout), ("stderr", &job.stderr)] {
            let stream = stream.lock();
            let text = String::from_utf8_lossy(&stream.bytes);
            let base = stream.total.saturating_sub(stream.bytes.len());
            let start = text.ceil_char_boundary(offset.saturating_sub(base).min(text.len()));
            let end = text.floor_char_boundary((start + limit.min(65536)).min(text.len()));
            value[name] = json!(dsh_types::safety_policy::redact_sensitive_text(
                &text[start..end]
            ));
            value[format!("{name}_bytes")] = json!(stream.total);
            value[format!("{name}_start_offset")] = json!(base + start);
            value[format!("{name}_next_offset")] = json!(base + end);
            value[format!("{name}_truncated")] = json!(stream.truncated);
            value[format!("{name}_complete")] = json!(stream.eof);
        }
        Ok(value)
    }
    pub fn cancel(&mut self, id: &str) -> Result<()> {
        let job = self
            .jobs
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("unknown job"))?;
        job.cancel.store(true, Ordering::Relaxed);
        if let Some(worker) = job.worker.take() {
            let _ = worker.join();
        }
        Ok(())
    }
    pub fn cancel_all(&mut self) {
        for job in self.jobs.values() {
            job.cancel.store(true, Ordering::Relaxed);
        }
        for job in self.jobs.values_mut() {
            if let Some(worker) = job.worker.take() {
                let _ = worker.join();
            }
        }
    }
    pub fn artifacts(&self) -> Vec<(String, Value)> {
        self.jobs
            .iter()
            .map(|(id, job)| {
                let mut value = job.state.lock().clone();
                for (name, stream) in [("stdout", &job.stdout), ("stderr", &job.stderr)] {
                    let stream = stream.lock();
                    value[name] = json!(String::from_utf8_lossy(&stream.bytes));
                    value[format!("{name}_truncated")] = json!(stream.truncated);
                    value[format!("{name}_complete")] = json!(stream.eof);
                }
                (id.clone(), value)
            })
            .collect()
    }

    /// New bytes from each stream since the given absolute offsets, unmasked.
    ///
    /// [`Self::snapshot`] is the model-facing reader: it masks secrets and
    /// cuts the requested window from its head. This one exists for the two
    /// callers that need the bytes as the command wrote them - echoing a
    /// running job to the terminal, and handing a finished job to
    /// `render_result`. **Anything derived from this that reaches the model
    /// must go through `redact_sensitive_text` first**; `snapshot` does that
    /// for its own callers, this does not.
    ///
    /// Offsets are absolute stream positions - the `*_next_offset` values
    /// `snapshot` reports. An offset older than what the ring still holds
    /// resumes at the oldest retained byte rather than failing; the caller
    /// learns that bytes were lost from `truncated` in `snapshot`.
    pub fn read_raw(&self, id: &str, stdout_from: usize, stderr_from: usize) -> Result<RawRead> {
        let job = self
            .jobs
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("unknown job"))?;

        // Slicing raw bytes can cut a multi-byte character in half, which is
        // fine for both callers: the terminal reassembles the stream in
        // order, and the final read starts at zero and goes through
        // `from_utf8_lossy`.
        let read_one = |stream: &Arc<Mutex<Output>>, from: usize| {
            let stream = stream.lock();
            let base = stream.total.saturating_sub(stream.bytes.len());
            let start = from.saturating_sub(base).min(stream.bytes.len());
            (stream.bytes[start..].to_vec(), stream.total, stream.eof)
        };

        let (stdout, stdout_next, stdout_complete) = read_one(&job.stdout, stdout_from);
        let (stderr, stderr_next, stderr_complete) = read_one(&job.stderr, stderr_from);

        Ok(RawRead {
            stdout,
            stderr,
            stdout_next,
            stderr_next,
            stdout_complete,
            stderr_complete,
        })
    }

    /// Drop all but the most recent `keep` finished jobs, returning their
    /// final snapshots so the caller can archive them.
    ///
    /// [`Self::start`] counts finished jobs against its 32-job ceiling. That
    /// is right for a task, which ends, and wrong for a shell session, which
    /// does not: without this the thirty-third `execute` of a session would
    /// fail. Only the interactive registry calls this - an agent task keeps
    /// every job so `finish` can still write them all out as artifacts.
    ///
    /// Running jobs are never reaped, however old, so the ceiling still
    /// bounds how many processes one session can have alive at once.
    pub fn reap_finished(&mut self, keep: usize) -> Vec<(String, Value)> {
        let mut finished: Vec<(u64, String)> = self
            .jobs
            .iter()
            .filter(|(_, job)| job.state.lock()["status"] != "running")
            .map(|(id, job)| (job.seq, id.clone()))
            .collect();

        if finished.len() <= keep {
            return Vec::new();
        }

        finished.sort_by_key(|(seq, _)| *seq);
        finished.truncate(finished.len() - keep);

        finished
            .into_iter()
            .filter_map(|(_, id)| {
                // Not `snapshot`: its window is capped at 64KiB, so archiving
                // through it would keep the head of a long log and throw away
                // the tail - the end of a build being the part anyone wants.
                let archived = self.archive_entry(&id)?;
                self.jobs.remove(&id);
                Some((id, archived))
            })
            .collect()
    }

    /// One finished job as a self-contained record, masked and complete.
    ///
    /// The same shape [`Self::artifacts`] produces for a task, built for a
    /// single job so the interactive registry can keep a reaped job readable.
    fn archive_entry(&self, id: &str) -> Option<Value> {
        let job = self.jobs.get(id)?;
        let raw = self.read_raw(id, 0, 0).ok()?;
        let mut value = job.state.lock().clone();
        value["job_id"] = json!(id);
        for (name, bytes, complete) in [
            ("stdout", &raw.stdout, raw.stdout_complete),
            ("stderr", &raw.stderr, raw.stderr_complete),
        ] {
            let text = String::from_utf8_lossy(bytes);
            value[name] = json!(dsh_types::safety_policy::redact_sensitive_text(&text));
            value[format!("{name}_complete")] = json!(complete);
        }
        Some(value)
    }
}
impl Drop for AgentJobs {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jobs_can_be_polled_and_cancelled_without_relaunch() {
        let mut jobs = AgentJobs::default();
        let mut command = Command::new("sh");
        command.args(["-c", "printf ready; sleep 10"]);
        let id = jobs.start(command, Duration::from_secs(20), None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let result = jobs.snapshot(&id, 0, 4096).unwrap();
            if result["stdout"] == "ready" {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        jobs.cancel(&id).unwrap();
        assert_eq!(jobs.snapshot(&id, 0, 4096).unwrap()["status"], "cancelled");
    }
    #[test]
    fn timeouts_and_exit_codes_are_distinct() {
        let mut jobs = AgentJobs::default();
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5"]);
        let id = jobs
            .start(command, Duration::from_millis(50), None)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let result = jobs.snapshot(&id, 0, 4096).unwrap();
            if result["status"] != "running" {
                assert_eq!(result["status"], "timed_out");
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(jobs.snapshot("not-owned", 0, 1).is_err());
    }

    /// Wait for one job to leave `running`, so a test never races the worker.
    fn settle(jobs: &AgentJobs, id: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while jobs.snapshot(id, 0, 64).unwrap()["status"] == "running" {
            assert!(Instant::now() < deadline, "job {id} never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn run_to_completion(jobs: &mut AgentJobs, script: &str) -> String {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        let id = jobs.start(command, Duration::from_secs(20), None).unwrap();
        settle(jobs, &id);
        id
    }

    /// `start` counts finished jobs against its ceiling, which is right for a
    /// task and wrong for a shell session. Without reaping, the thirty-third
    /// `execute` of a session fails.
    #[test]
    fn finished_jobs_are_reaped_so_the_limit_is_not_reached() {
        let mut jobs = AgentJobs::default();
        for _ in 0..40 {
            jobs.reap_finished(8);
            run_to_completion(&mut jobs, "true");
        }
        assert!(jobs.jobs.len() <= 9, "leaked jobs: {}", jobs.jobs.len());
    }

    /// The archive a reap hands back has to carry the whole log: `snapshot`
    /// caps its window at 64KiB, which would keep the head of a build and
    /// throw away the failure at the end.
    #[test]
    fn a_reaped_job_is_archived_whole_rather_than_through_the_snapshot_window() {
        let mut jobs = AgentJobs::default();
        let id = run_to_completion(&mut jobs, "printf 'x%.0s' $(seq 1 70000)");
        jobs.reap_finished(8);
        run_to_completion(&mut jobs, "true");

        let archived = jobs.reap_finished(0);
        let (_, entry) = archived
            .iter()
            .find(|(archived_id, _)| archived_id == &id)
            .expect("reaped job is archived");
        assert_eq!(entry["stdout"].as_str().unwrap().len(), 70000);
        assert_eq!(entry["status"], "exited");
    }

    /// A running job is never reaped, however old, so the ceiling still
    /// bounds how many processes one session can have alive.
    #[test]
    fn a_running_job_survives_a_reap() {
        let mut jobs = AgentJobs::default();
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 10"]);
        let running = jobs.start(command, Duration::from_secs(20), None).unwrap();
        run_to_completion(&mut jobs, "true");

        jobs.reap_finished(0);

        assert!(jobs.snapshot(&running, 0, 64).is_ok());
        jobs.cancel(&running).unwrap();
    }

    #[test]
    fn read_raw_returns_only_new_bytes_from_an_offset() {
        let mut jobs = AgentJobs::default();
        let id = run_to_completion(&mut jobs, "printf abcdef");

        let first = jobs.read_raw(&id, 0, 0).unwrap();
        assert_eq!(first.stdout, b"abcdef");
        assert_eq!(first.stdout_next, 6);
        assert!(first.stdout_complete);

        let second = jobs.read_raw(&id, first.stdout_next, 0).unwrap();
        assert!(second.stdout.is_empty());
        assert_eq!(second.stdout_next, 6);

        let partial = jobs.read_raw(&id, 3, 0).unwrap();
        assert_eq!(partial.stdout, b"def");
    }

    /// `read_raw` is the unmasked reader by contract; `snapshot` is the one
    /// that redacts. A caller that mixed them up would leak a secret into the
    /// conversation, so pin the difference.
    #[test]
    fn read_raw_does_not_mask_while_snapshot_does() {
        let mut jobs = AgentJobs::default();
        let id = run_to_completion(&mut jobs, "printf 'export AWS_SECRET_ACCESS_KEY=abcd1234'");

        let raw = String::from_utf8(jobs.read_raw(&id, 0, 0).unwrap().stdout).unwrap();
        let masked = jobs.snapshot(&id, 0, 4096).unwrap()["stdout"]
            .as_str()
            .unwrap()
            .to_string();

        assert!(raw.contains("abcd1234"), "{raw}");
        assert!(!masked.contains("abcd1234"), "{masked}");
    }
}
