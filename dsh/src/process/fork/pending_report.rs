//! A single, movable owner for a deferred exec report and its diagnostic route.
use super::*;
use std::os::fd::OwnedFd;

#[derive(Debug)]
pub(crate) struct PendingExecReport {
    pub(crate) pid: Option<Pid>,
    reader: OwnedFd,
    destination: OwnedFd,
    command: String,
    full_proxy: bool,
    observer: Option<SharedOutputObserver>,
    bytes: [u8; std::mem::size_of::<ChildExecError>()],
    filled: usize,
}

impl PendingExecReport {
    pub(super) fn new(
        reader: OwnedFd,
        process: &Process,
        target: Option<&ParentExecDiagnostic<'_>>,
    ) -> Result<Self> {
        let flags = nix::fcntl::fcntl(&reader, nix::fcntl::FcntlArg::F_GETFL)?;
        nix::fcntl::fcntl(
            &reader,
            nix::fcntl::FcntlArg::F_SETFL(
                nix::fcntl::OFlag::from_bits_truncate(flags) | nix::fcntl::OFlag::O_NONBLOCK,
            ),
        )?;
        let destination = target.map_or(process.stderr, |target| target.caller.as_raw_fd());
        let destination = unsafe { BorrowedFd::borrow_raw(destination) }.try_clone_to_owned()?;
        Ok(Self {
            pid: None,
            reader,
            destination,
            command: process.cmd.clone(),
            full_proxy: target.is_some(),
            observer: target.and_then(|target| target.observer.cloned()),
            bytes: [0; std::mem::size_of::<ChildExecError>()],
            filled: 0,
        })
    }

    /// Returns true only when the report owner can be retired. Never waits,
    /// never consumes process status, and preserves partial records on EAGAIN.
    pub(super) fn drain(&mut self, captured: &mut Vec<u8>) -> bool {
        let error = loop {
            if self.filled == self.bytes.len() {
                break None;
            }
            let count = unsafe {
                libc::read(
                    self.reader.as_raw_fd(),
                    self.bytes[self.filled..].as_mut_ptr().cast(),
                    self.bytes.len() - self.filled,
                )
            };
            if count == 0 {
                break None;
            }
            if count > 0 {
                self.filled += count as usize;
                continue;
            }
            let error = std::io::Error::last_os_error();
            match error.kind() {
                std::io::ErrorKind::Interrupted => continue,
                std::io::ErrorKind::WouldBlock => return false,
                _ => break Some(error),
            }
        };
        let mut bytes = &self.bytes[..self.filled];
        let mut error = error;
        let target = self.full_proxy.then(|| ParentExecDiagnostic {
            caller: unsafe { BorrowedFd::borrow_raw(self.destination.as_raw_fd()) },
            captured,
            observer: self.observer.as_ref(),
        });
        drain_exec_error_with_reader(
            |buffer| {
                if let Some(error) = error.take() {
                    return Err(error);
                }
                std::io::Read::read(&mut bytes, buffer)
            },
            &self.command,
            self.destination.as_raw_fd(),
            &[],
            target,
        );
        true
    }
}

impl crate::process::Job {
    pub(crate) fn drain_exec_reports(&mut self) {
        self.exec_reports
            .retain_mut(|report| !report.drain(&mut self.parent_exec_diagnostics));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    fn fixture() -> (PendingExecReport, std::fs::File, std::fs::File) {
        let (reader, writer) = cloexec_pipe().unwrap();
        let destination = tempfile::tempfile().unwrap();
        let mut process = Process::new("deferred-denied".into(), vec![]);
        process.stderr = destination.as_raw_fd();
        (
            PendingExecReport::new(reader, &process, None).unwrap(),
            writer.into(),
            destination,
        )
    }
    #[test]
    fn partial_record_waits_without_diagnostic_then_delivers_once() {
        let (mut report, mut writer, mut destination) = fixture();
        let record = ChildExecError {
            stage: STAGE_EXECVE,
            errno: libc::EACCES,
        };
        // Initialize the complete wire image, including padding.
        let mut bytes = [0; std::mem::size_of::<ChildExecError>()];
        bytes[0] = record.stage;
        let offset = std::mem::offset_of!(ChildExecError, errno);
        bytes[offset..offset + 4].copy_from_slice(&record.errno.to_ne_bytes());
        writer.write_all(&bytes[..1]).unwrap();
        assert!(!report.drain(&mut Vec::new()));
        assert_eq!(destination.metadata().unwrap().len(), 0);
        writer.write_all(&bytes[1..]).unwrap();
        assert!(report.drain(&mut Vec::new()));
        drop(report);
        use std::io::{Seek, SeekFrom};
        destination.seek(SeekFrom::Start(0)).unwrap();
        let mut text = String::new();
        destination.read_to_string(&mut text).unwrap();
        assert_eq!(
            text.lines()
                .filter(|line| line.contains("Permission denied"))
                .count(),
            1
        );
    }
    #[test]
    fn success_retires_the_only_retained_diagnostic_writer() {
        let (reader, writer) = cloexec_pipe().unwrap();
        let (diagnostic_read, diagnostic_write) = cloexec_pipe().unwrap();
        let mut process = Process::new("success".into(), vec![]);
        process.stderr = diagnostic_write.as_raw_fd();
        let mut report = PendingExecReport::new(reader, &process, None).unwrap();
        drop(diagnostic_write);
        assert!(!report.drain(&mut Vec::new()));
        drop(writer); // CLOEXEC success analogue: no error record.
        assert!(report.drain(&mut Vec::new()));
        drop(report);
        let mut bytes = Vec::new();
        std::fs::File::from(diagnostic_read)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(
            bytes.is_empty(),
            "success emits no diagnostic and permits EOF"
        );
    }
    #[test]
    fn detach_pending_report_without_wait_authority_keeps_real_state_and_owner() {
        let (report, _writer, _destination) = fixture();
        let mut job =
            crate::process::Job::new("stale startup owner".into(), nix::unistd::getpgrp());
        // Parent PID cannot be our child: this pins a deterministic ECHILD
        // without spawning, reaping, or signalling an unrelated process.
        let mut process = Process::new("fixture".into(), vec![]);
        process.pid = Some(nix::unistd::getppid());
        job.set_process(crate::process::JobProcess::Command(process));
        job.exec_reports.push(report);
        let error =
            crate::process::job_wait::settle_startup_reports_for_detach(&mut job).unwrap_err();
        assert!(error.to_string().contains("without wait authority"));
        assert_eq!(job.lifecycle_state(), crate::process::ProcessState::Running);
        assert_eq!(job.final_exit_status(), None);
        assert_eq!(job.exec_reports.len(), 1);
    }
}
