//! Scratch-session ownership: two live group leaders and an explicit EOF cleanup.
//!
//! Each anchor is a direct child kept unreaped by the supervisor. On control
//! pipe EOF it signals its OWN group with kill(0), so delivery never resolves a
//! sampled PID/PGID. The shell worker and external job use separate groups.

use super::pty_pump::{Outcome, PtyPump, scratch_pty};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

static CLEANUP_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
extern "C" fn request_cleanup(_: i32) {
    CLEANUP_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

const PREFIX: &str = "process::foreground_handoff_tests::supervisor::";

/// Owns the supervisor identity and keeps pumping the scratch master during
/// cooperative cleanup and, only after that grace period, direct termination.
struct OwnedChild {
    child: Child,
    pump: PtyPump,
    mode: String,
    log: std::path::PathBuf,
    total_deadline: Instant,
    status: Option<ExitStatus>,
}

impl OwnedChild {
    fn partial_log(&self) {
        let mut bytes = Vec::new();
        if let Ok(file) = File::open(&self.log) {
            let _ = file.take(16 * 1024).read_to_end(&mut bytes);
        }
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(
            stderr,
            "handoff-harness case={} partial scratch log before teardown:",
            self.mode
        );
        let _ = stderr.write_all(&bytes);
    }

    fn finish(&mut self, request_cleanup: bool) -> Option<ExitStatus> {
        if self.status.is_some() {
            return self.status;
        }
        if request_cleanup && !exited(&self.child) {
            self.partial_log();
            self.pump.live(&self.mode, "cleanup-request");
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(self.child.id() as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
            let grace = (Instant::now() + Duration::from_secs(7))
                .min(self.total_deadline - Duration::from_secs(1));
            if let Outcome::IoError(_) = self.pump.until(grace, false, || exited(&self.child)) {
                self.pump.live(&self.mode, "cleanup-io-error");
                observe_until(&self.child, grace);
            }
            if !exited(&self.child) {
                self.pump.live(&self.mode, "cleanup-direct-kill");
                let _ = self.child.kill(); // still-owned unreaped direct child
                if let Outcome::IoError(_) = self
                    .pump
                    .until(self.total_deadline, false, || exited(&self.child))
                {
                    self.pump.live(&self.mode, "termination-io-error");
                    observe_until(&self.child, self.total_deadline);
                }
            }
        }
        // No unconditional blocking Child::wait after an observation timeout.
        if exited(&self.child) {
            let _ = self.pump.until(Instant::now(), false, || true);
            self.status = self.child.wait().ok();
            self.pump.live(&self.mode, "cleanup-reaped");
        } else {
            self.pump.live(&self.mode, "cleanup-unconfirmed");
        }
        self.status
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.status.is_none() {
            let _ = self.finish(true);
        }
    }
}

/// Observe exit without releasing the PID identity held by our direct child.
fn exited(child: &Child) -> bool {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        )
    };
    result == 0 && unsafe { info.si_pid() } == child.id() as i32
}

fn observe_until(child: &Child, deadline: Instant) -> bool {
    loop {
        if exited(child) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        unsafe { libc::poll(std::ptr::null_mut(), 0, 2) };
    }
}

struct Supervisor {
    worker: Option<Child>,
    shell_anchor: Option<Child>,
    job_anchor: Option<Child>,
    lifetime: Option<File>,
    cleaned: bool,
    child_start_writer: Option<std::os::fd::OwnedFd>,
    drop_proof: Option<std::path::PathBuf>,
}

struct Cleanup {
    worker: Option<ExitStatus>,
    shell_anchor: Option<ExitStatus>,
    job_anchor: Option<ExitStatus>,
    lifetime_eof: bool,
}

impl Supervisor {
    fn cleanup(&mut self) -> Cleanup {
        self.cleaned = true;
        if let Some(worker) = &mut self.worker {
            if !exited(worker) {
                let _ = worker.kill();
            }
            if !observe_until(worker, Instant::now() + Duration::from_secs(2)) {
                // No successful cleanup claim or group snapshot while the
                // spawning worker has not been observed quiescent.
                return Cleanup {
                    worker: None,
                    shell_anchor: None,
                    job_anchor: None,
                    lifetime_eof: false,
                };
            }
        }
        // Child may still be in the shell group before setpgid, or may have
        // moved to the job group. Quiesce worker spawning first, then signal
        // shell -> job in that order. BOTH leader PIDs remain unreaped until
        // after delivery and EOF proof; no identity probe authorizes a signal.
        for anchor in [&self.shell_anchor, &self.job_anchor].into_iter().flatten() {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(anchor.id() as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        for child in [&self.worker, &self.shell_anchor, &self.job_anchor]
            .into_iter()
            .flatten()
        {
            observe_until(child, deadline);
        }
        let lifetime_eof = self.lifetime.as_mut().is_none_or(|reader| {
            let fd = reader.as_raw_fd();
            drain_to_eof(reader, fd)
        });
        fn reap(child: &mut Option<Child>) -> Option<ExitStatus> {
            child
                .as_mut()
                .filter(|child| exited(child))
                .and_then(|child| child.wait().ok())
        }
        Cleanup {
            worker: reap(&mut self.worker),
            shell_anchor: reap(&mut self.shell_anchor),
            job_anchor: reap(&mut self.job_anchor),
            lifetime_eof,
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if !self.cleaned {
            let cleanup = self.cleanup();
            if cleanup.lifetime_eof
                && cleanup.worker.is_some()
                && cleanup
                    .shell_anchor
                    .is_some_and(|status| status.signal() == Some(libc::SIGKILL))
                && cleanup
                    .job_anchor
                    .is_some_and(|status| status.signal() == Some(libc::SIGKILL))
                && let Some(path) = self.drop_proof.take()
            {
                let _ = std::fs::write(path, b"ordered-signals+dedicated-eof+reaped");
            }
        }
    }
}

pub(super) fn run_scratch(mode: &str) {
    let root = tempfile::tempdir().unwrap();
    let pty = scratch_pty().unwrap();
    let master = File::from(pty.master);
    let slave = File::from(pty.slave);
    let proof = root.path().join("proof");
    let log = root.path().join("log");
    let log_file = File::create(&log).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            &format!("{PREFIX}scratch_supervisor"),
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("DOGESH_ATUIN_DUAL_WRITE", "0")
        .env("TERM", "dumb")
        .env("DSH_HANDOFF_ROLE", "supervisor")
        .env("DSH_HANDOFF_CASE", mode)
        .env("DSH_HANDOFF_PROOF", &proof)
        .env("DSH_HANDOFF_WORKER_PROOF", root.path().join("worker-proof"))
        .env("DSH_HANDOFF_FAULT_READY", root.path().join("fault-ready"))
        .current_dir(root.path())
        .stdin(Stdio::from(slave))
        .stdout(Stdio::from(log_file.try_clone().unwrap()))
        .stderr(Stdio::from(log_file));
    let controlling = mode != "no-ctty";
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0
                || (controlling && libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0)
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // One absolute 20s bound reserves the existing 7s cooperative cleanup
    // window plus 1s termination observation; feed never runs before it.
    if mode.starts_with("preexec-") {
        let mut modes = nix::sys::termios::tcgetattr(&master).unwrap();
        modes
            .local_flags
            .insert(nix::sys::termios::LocalFlags::ISIG);
        modes.control_chars[nix::sys::termios::SpecialCharacterIndices::VSUSP as usize] = 26;
        nix::sys::termios::tcsetattr(&master, nix::sys::termios::SetArg::TCSANOW, &modes).unwrap();
    }
    let total_deadline = Instant::now() + Duration::from_secs(20);
    let pump = PtyPump::new(
        master,
        b"foreground-ready\n".repeat(if mode == "fd-budget" { 96 } else { 1 }),
    )
    .unwrap();
    pump.live(mode, "spawn-start");
    let child = command.spawn().unwrap();
    let mut supervisor = OwnedChild {
        child,
        pump,
        mode: mode.into(),
        log: log.clone(),
        total_deadline,
        status: None,
    };
    supervisor.pump.live(mode, "spawned");
    let run_deadline = total_deadline - Duration::from_secs(8);
    let mut pump_failure = None;
    if mode.starts_with("preexec-") {
        let ready = root.path().join("preexec-ready");
        let observed = supervisor
            .pump
            .until(run_deadline, false, || ready.exists());
        if observed != Outcome::Ready {
            supervisor.partial_log();
        }
        assert_eq!(
            observed,
            Outcome::Ready,
            "child must reach raw preexec seam"
        );
        if !mode.starts_with("preexec-admission") && !mode.starts_with("preexec-direct") {
            supervisor
                .pump
                .control_byte(if mode == "preexec-int" { 3 } else { 26 });
        }
        eprintln!(
            "preexec child ready; signal action: {}",
            if mode.starts_with("preexec-admission") || mode.starts_with("preexec-direct") {
                "SIGSTOP to ready owned child"
            } else if mode == "preexec-int" {
                "real terminal Ctrl-C"
            } else {
                "real terminal Ctrl-Z"
            }
        );
    }
    let status = if mode == "worker-parent-unwind" {
        let ready = root.path().join("fault-ready");
        let deadline = (Instant::now() + Duration::from_secs(3)).min(run_deadline);
        let observed = supervisor.pump.until(deadline, true, || ready.exists());
        supervisor.pump.live(mode, "parent-ready-observation");
        assert_eq!(observed, Outcome::Ready);
        assert_eq!(std::fs::read(ready).unwrap(), b"forked");
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _supervisor = supervisor;
            panic!("injected outer parent unwind with live worker/raw child");
        }));
        assert!(panicked.is_err());
        None
    } else {
        let outcome = supervisor
            .pump
            .until(run_deadline, true, || exited(&supervisor.child));
        supervisor.pump.live(
            mode,
            match outcome {
                Outcome::Ready => "run-exited",
                Outcome::Deadline => "run-deadline",
                Outcome::IoError(_) => "run-io-error",
            },
        );
        let failed = outcome != Outcome::Ready;
        if failed {
            pump_failure = Some(format!("{outcome:?}"));
            supervisor.partial_log(); // emit bounded evidence BEFORE teardown
        }
        let status = supervisor.finish(failed);
        drop(supervisor);
        status
    };
    let output = std::fs::read_to_string(&log).unwrap();
    eprintln!("{output}");
    assert!(
        pump_failure.is_none(),
        "{mode}: PTY pump failed: {pump_failure:?}"
    );
    assert!(
        mode == "worker-parent-unwind" || status.is_some_and(|status| status.success()),
        "{mode}: {status:?}\n{output}"
    );
    assert_eq!(
        std::fs::read(&proof).unwrap(),
        b"ok",
        "supervisor cleanup actually completed"
    );
    if mode == "exec-failure" {
        assert!(output.contains("Permission denied"), "{output}");
    }
    if mode == "handoff-abort" {
        assert!(
            output.contains("failed foreground terminal handoff"),
            "{output}"
        );
    }
}

fn spawn_anchor() -> Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &format!("{PREFIX}scratch_anchor"), "--nocapture"])
        .env("DSH_HANDOFF_ROLE", "anchor")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().unwrap()
}

#[test]
fn scratch_anchor() {
    if std::env::var("DSH_HANDOFF_ROLE").as_deref() != Ok("anchor") {
        return;
    }
    assert_eq!(nix::unistd::getpgrp(), nix::unistd::getpid());
    // Pin the supervisor-owned group identity through the Ctrl-C fixture.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
    // No writer exists in this process. The supervisor's CLOEXEC ChildStdin
    // is not inherited across worker exec. On EOF, our own membership pins the
    // signal target through kernel delivery, even if the supervisor died.
    let mut byte = 0u8;
    loop {
        let read = unsafe { libc::read(0, (&mut byte as *mut u8).cast(), 1) };
        if read < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        break;
    }
    unsafe {
        libc::kill(0, libc::SIGKILL);
        libc::_exit(99);
    }
}

fn drain_to_eof(reader: &mut impl Read, fd: i32) -> bool {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return false;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bytes = [0; 4096];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => return true,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return false,
        }
        if Instant::now() >= deadline {
            return false;
        }
        unsafe { libc::poll(std::ptr::null_mut(), 0, 2) };
    }
}

#[test]
fn scratch_supervisor() {
    if std::env::var("DSH_HANDOFF_ROLE").as_deref() != Ok("supervisor") {
        return;
    }
    let mode = std::env::var("DSH_HANDOFF_CASE").unwrap();
    // Process-local handler in this env-cleared scratch supervisor only.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            request_cleanup as *const () as libc::sighandler_t,
        );
    }
    // Construct the aggregate owner before each subsequent fallible step, so
    // supervisor unwind runs the same ordered cleanup as ordinary outcomes.
    let mut owner = Supervisor {
        worker: None,
        shell_anchor: None,
        job_anchor: Some(spawn_anchor()),
        lifetime: None,
        cleaned: false,
        child_start_writer: None,
        drop_proof: None,
    };
    owner.shell_anchor = Some(spawn_anchor());
    let job_group = owner.job_anchor.as_ref().unwrap().id() as i32;
    let shell_group = owner.shell_anchor.as_ref().unwrap().id() as i32;
    unsafe {
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
    }
    if mode != "no-ctty" {
        nix::unistd::tcsetpgrp(
            unsafe { BorrowedFd::borrow_raw(0) },
            nix::unistd::Pid::from_raw(shell_group),
        )
        .unwrap();
    }
    // Dedicated inherited lifetime pipe, created AFTER anchors. It is not a
    // stdio source, so OutputOnly PTY rewiring cannot close its child writer.
    let (reader, writer) = super::super::io::cloexec_pipe().unwrap();
    owner.lifetime = Some(File::from(reader));
    let lifetime_fd = writer.as_raw_fd();
    // Fault cases hold the raw child BEFORE its first setpgid. Only this
    // supervisor owns the writer, retained until ordered group cleanup ends.
    let child_start_reader = if mode.starts_with("worker-") || mode == "supervisor-panic" {
        let (reader, writer) = super::super::io::cloexec_pipe().unwrap();
        owner.child_start_writer = Some(writer);
        Some(reader)
    } else {
        None
    };
    let start_fd = child_start_reader.as_ref().map_or(-1, AsRawFd::as_raw_fd);
    let preexec = if mode.starts_with("preexec-") {
        let (ready_read, ready_write) = super::super::io::cloexec_pipe().unwrap();
        let (hold_read, hold_write) = super::super::io::cloexec_pipe().unwrap();
        Some((ready_read, ready_write, hold_read, hold_write))
    } else {
        None
    };
    let ready_fd = preexec.as_ref().map_or(-1, |p| p.1.as_raw_fd());
    let hold_fd = preexec.as_ref().map_or(-1, |p| p.2.as_raw_fd());
    let stdin = unsafe { BorrowedFd::borrow_raw(0) }
        .try_clone_to_owned()
        .unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "process::foreground_handoff_tests::scratch_session",
            "--nocapture",
        ])
        .env("DSH_HANDOFF_ROLE", "worker")
        .env("DSH_PREEXEC_READY_FD", ready_fd.to_string())
        .env("DSH_PREEXEC_HOLD_FD", hold_fd.to_string())
        .env("DSH_HANDOFF_SUPERVISOR", std::process::id().to_string())
        .env("DSH_HANDOFF_JOB_GROUP", job_group.to_string())
        .env("DSH_HANDOFF_LIFETIME_FD", lifetime_fd.to_string())
        .env("DSH_HANDOFF_CHILD_START_FD", start_fd.to_string())
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, shell_group) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            for fd in [ready_fd, hold_fd] {
                if fd >= 0 {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
            }
            if start_fd >= 0 {
                let flags = libc::fcntl(start_fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(start_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            let flags = libc::fcntl(lifetime_fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(lifetime_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    owner.worker = Some(command.spawn().unwrap());
    let mut preexec_hold = preexec.map(|(read, write, hold_read, hold_write)| {
        drop(write);
        drop(hold_read);
        let mut ready = File::from(read);
        super::pty_pump::nonblocking(&ready).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut bytes = [0; std::mem::size_of::<libc::pid_t>()];
        let mut filled = 0;
        loop {
            match ready.read(&mut bytes[filled..]) {
                Ok(count) if count > 0 => {
                    filled += count;
                    if filled == bytes.len() {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                result => panic!("preexec readiness: {result:?}"),
            }
            assert!(Instant::now() < deadline, "raw preexec readiness deadline");
            unsafe {
                libc::poll(std::ptr::null_mut(), 0, 1);
            }
        }
        if mode.starts_with("preexec-admission") || mode.starts_with("preexec-direct") {
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(libc::pid_t::from_ne_bytes(bytes)),
                nix::sys::signal::Signal::SIGSTOP,
            )
            .unwrap();
        }
        std::fs::write("preexec-ready", b"ready").unwrap();
        hold_write
    });
    drop(child_start_reader);
    drop(writer); // Only worker + its real fork/exec children own writers now.
    if mode == "supervisor-panic" {
        let ready = std::path::PathBuf::from(std::env::var_os("DSH_HANDOFF_FAULT_READY").unwrap());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < deadline {
            unsafe { libc::poll(std::ptr::null_mut(), 0, 2) };
        }
        assert_eq!(std::fs::read(&ready).unwrap(), b"forked");
        let receipt = ready.with_extension("drop-proof");
        owner.drop_proof = Some(receipt.clone());
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _owner = owner;
            panic!("injected supervisor unwind with live worker and pre-setpgid external child");
        }));
        assert!(panicked.is_err());
        assert_eq!(
            std::fs::read(receipt).unwrap(),
            b"ordered-signals+dedicated-eof+reaped"
        );
        eprintln!("cleanup case=supervisor-panic Drop: ordered-signals+dedicated-eof+reaped");
        std::fs::write(std::env::var_os("DSH_HANDOFF_PROOF").unwrap(), b"ok").unwrap();
        return;
    }
    let timeout = if mode == "worker-timeout" {
        Duration::from_secs(1)
    } else {
        Duration::from_secs(12)
    };
    let deadline = Instant::now() + timeout;
    let timed_out = loop {
        if preexec_hold.is_some() && std::path::Path::new("launch-returned").exists() {
            drop(preexec_hold.take());
            std::fs::write("preexec-released", b"released").unwrap();
        }
        if exited(owner.worker.as_ref().unwrap()) {
            break false;
        }
        if CLEANUP_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) || Instant::now() >= deadline
        {
            break true;
        }
        unsafe { libc::poll(std::ptr::null_mut(), 0, 2) };
    };
    let cleanup = owner.cleanup(); // Reaps only AFTER both ordered signals/EOF.
    eprintln!(
        "cleanup case={mode} worker={:?} timeout={timed_out} job_anchor={:?} shell_anchor={:?} dedicated_lifetime_eof={}",
        cleanup.worker, cleanup.job_anchor, cleanup.shell_anchor, cleanup.lifetime_eof
    );
    assert!(
        cleanup
            .job_anchor
            .is_some_and(|status| status.signal() == Some(libc::SIGKILL))
    );
    assert!(
        cleanup
            .shell_anchor
            .is_some_and(|status| status.signal() == Some(libc::SIGKILL))
    );
    assert!(
        cleanup.lifetime_eof,
        "all owned auxiliary lifetime writers must close, including PTY children"
    );
    let status = cleanup.worker.expect("worker reaped after group cleanup");
    if mode.starts_with("worker-") {
        assert_eq!(
            std::fs::read(std::env::var_os("DSH_HANDOFF_FAULT_READY").unwrap()).unwrap(),
            b"forked"
        );
        match mode.as_str() {
            "worker-panic" => assert!(!status.success() && !timed_out),
            "worker-abort" => assert_eq!(status.signal(), Some(libc::SIGABRT)),
            "worker-timeout" => assert!(timed_out),
            "worker-parent-unwind" => {
                assert!(timed_out && CLEANUP_REQUESTED.load(std::sync::atomic::Ordering::SeqCst))
            }
            _ => panic!("unknown fault"),
        }
    } else {
        assert!(status.success(), "worker failed: {status:?}");
        assert_eq!(
            std::fs::read(std::env::var_os("DSH_HANDOFF_WORKER_PROOF").unwrap()).unwrap(),
            b"ok"
        );
    }
    std::fs::write(std::env::var_os("DSH_HANDOFF_PROOF").unwrap(), b"ok").unwrap();
}
