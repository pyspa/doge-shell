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
}
#[derive(Default)]
pub struct AgentJobs {
    jobs: HashMap<String, Job>,
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
        self.jobs.insert(
            id.clone(),
            Job {
                state,
                cancel,
                stdout,
                stderr,
                worker: Some(worker),
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
}
