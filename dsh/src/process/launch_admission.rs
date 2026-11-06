//! Job-owned admission: all stage PIDs and signal defaults precede terminal handoff.
use super::{Job, io::cloexec_pipe};
use anyhow::{Context, Result};
use nix::unistd::Pid;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd, RawFd};

#[derive(Debug, Default)]
pub(crate) struct LaunchAdmission {
    stages: Vec<AdmittedStage>,
    stop_requested: bool,
    released: bool,
    // Foreground intent alone is insufficient after a stopped launch returns
    // to the prompt. Only an active foreground wait may hand off the terminal.
    pub(crate) foreground_wait_active: bool,
}
#[derive(Debug)]
struct AdmittedStage {
    pid: Pid,
    gate: Option<OwnedFd>,
    ready: OwnedFd,
    acknowledged: bool,
    completed: bool,
    helper: bool,
}

pub(crate) struct StageAdmission {
    pub gate_read: OwnedFd,
    pub gate_write: OwnedFd,
    pub ready_read: OwnedFd,
    pub ready_write: OwnedFd,
    helper: bool,
}
impl StageAdmission {
    pub fn new() -> Result<Self> {
        let (gate_read, gate_write) = cloexec_pipe()?;
        let (ready_read, ready_write) = cloexec_pipe()?;
        let flags = nix::fcntl::fcntl(&ready_read, nix::fcntl::FcntlArg::F_GETFL)?;
        nix::fcntl::fcntl(
            &ready_read,
            nix::fcntl::FcntlArg::F_SETFL(
                nix::fcntl::OFlag::from_bits_truncate(flags) | nix::fcntl::OFlag::O_NONBLOCK,
            ),
        )?;
        Ok(Self {
            gate_read,
            gate_write,
            ready_read,
            ready_write,
            helper: false,
        })
    }
    pub fn for_helper() -> Result<Self> {
        let mut stage = Self::new()?;
        stage.helper = true;
        Ok(stage)
    }
    pub fn raw(&self) -> [RawFd; 4] {
        [
            self.gate_read.as_raw_fd(),
            self.gate_write.as_raw_fd(),
            self.ready_read.as_raw_fd(),
            self.ready_write.as_raw_fd(),
        ]
    }
}
impl LaunchAdmission {
    pub fn admit(&mut self, pid: Pid, stage: StageAdmission) {
        self.stages.push(AdmittedStage {
            pid,
            gate: Some(stage.gate_write),
            ready: stage.ready_read,
            acknowledged: false,
            completed: false,
            helper: stage.helper,
        });
    }
}
impl Job {
    pub(crate) fn has_unreleased_admission(&self) -> bool {
        self.launch_admission
            .as_ref()
            .is_some_and(|admission| !admission.released)
    }

    /// Ready acknowledgements precede release. Startup ends only on external
    /// exec-report resolution or a helper's consumed-token acknowledgement.
    /// A stopped startup PID propagates a real stop; ordinary post-startup
    /// partial stops retain the usual canonical wait semantics.
    pub(crate) fn progress_launch_admission(&mut self) -> Result<()> {
        let Some(mut admission) = self.launch_admission.take() else {
            return Ok(());
        };
        let result = (|| -> Result<bool> {
            if self.is_process_tree_completed() {
                return Ok(true);
            }
            for stage in &mut admission.stages {
                if !stage.helper {
                    stage.completed = !self
                        .exec_reports
                        .iter()
                        .any(|report| report.pid == Some(stage.pid));
                }
                if stage.completed {
                    stage.acknowledged = true;
                    continue;
                }
                loop {
                    let mut byte = 0u8;
                    let count = unsafe {
                        libc::read(stage.ready.as_raw_fd(), (&mut byte as *mut u8).cast(), 1)
                    };
                    if count == 1 {
                        if !stage.acknowledged && byte == b'R' {
                            stage.acknowledged = true;
                        } else if stage.helper && admission.released && byte == b'C' {
                            stage.completed = true;
                        } else {
                            anyhow::bail!("invalid launch readiness");
                        }
                        if !stage.helper || stage.completed {
                            break;
                        }
                        continue;
                    }
                    // EOF before acknowledgement means setup failed, never
                    // synthetic completion. Canonical wait still owns status.
                    if count == 0 {
                        stage.acknowledged = true;
                        if stage.helper {
                            stage.completed = true;
                        }
                        break;
                    }
                    let error = std::io::Error::last_os_error();
                    match error.kind() {
                        std::io::ErrorKind::Interrupted => continue,
                        std::io::ErrorKind::WouldBlock => break,
                        _ => return Err(error).context("read launch readiness"),
                    }
                }
            }
            let stopped_startup = admission.stages.iter().any(|stage| {
                if stage.completed {
                    return false;
                }
                let mut node = self.process.as_deref();
                while let Some(process) = node {
                    if process.get_pid() == Some(stage.pid) {
                        return matches!(process.get_state(), super::ProcessState::Stopped(..));
                    }
                    node = process.next_process();
                }
                false
            });
            if (!admission.released && self.has_stopped_process()) || stopped_startup {
                if !admission.stop_requested {
                    self.signal(nix::sys::signal::Signal::SIGSTOP)?;
                    #[cfg(test)]
                    super::foreground_handoff_tests::record_startup_stop();
                    admission.stop_requested = true;
                }
                return Ok(false);
            }
            if !self.has_stopped_process() {
                admission.stop_requested = false;
            }
            if admission.released {
                return Ok(admission.stages.iter().all(|stage| stage.completed));
            }
            if admission.stages.iter().any(|stage| !stage.acknowledged) {
                return Ok(false);
            }
            #[cfg(test)]
            super::foreground_handoff_tests::admission_ready_checkpoint();
            if self.foreground
                && admission.foreground_wait_active
                && super::job_wait::can_handoff_terminal()
            {
                #[cfg(test)]
                if super::foreground_handoff_tests::reject_handoff() {
                    return Ok(true);
                }
                let pgid = self.pgid.context("admitted job missing group")?;
                if let Err(error) = nix::unistd::tcsetpgrp(
                    unsafe { BorrowedFd::borrow_raw(crate::shell::SHELL_TERMINAL) },
                    pgid,
                ) {
                    tracing::debug!("launch terminal handoff failed: {error}");
                    return Ok(true); // EOF cancels children, canonical wait retains status.
                }
            }
            for stage in &mut admission.stages {
                if let Some(gate) = stage.gate.take() {
                    loop {
                        match nix::unistd::write(&gate, b"F") {
                            Err(nix::errno::Errno::EINTR) => continue,
                            Ok(1) | Err(nix::errno::Errno::EPIPE) => break,
                            result => {
                                anyhow::bail!("release admission for {}: {result:?}", stage.pid)
                            }
                        }
                    }
                }
            }
            admission.released = true;
            Ok(false)
        })();
        if matches!(result, Ok(false)) {
            self.launch_admission = Some(admission);
        }
        result.map(|_| ())
    }
}
