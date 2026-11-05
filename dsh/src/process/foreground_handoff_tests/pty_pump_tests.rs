//! Bounded scratch-PTY feed/drain; every descriptor here belongs to a test.
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Ready,
    Deadline,
    IoError(std::io::ErrorKind),
}

pub(super) struct PtyPump {
    master: File,
    input: Vec<u8>,
    pub(super) fed: usize,
    pub(super) drained: usize,
    pub(super) blocked: usize,
}

pub(super) fn nonblocking(file: &File) -> std::io::Result<()> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

impl PtyPump {
    pub(super) fn new(master: File, input: Vec<u8>) -> std::io::Result<Self> {
        nonblocking(&master)?;
        Ok(Self {
            master,
            input,
            fed: 0,
            drained: 0,
            blocked: 0,
        })
    }

    pub(super) fn live(&self, mode: &str, phase: &str) {
        // An explicit stderr writer bypasses libtest's thread-local capture.
        let _ = writeln!(
            std::io::stderr().lock(),
            "handoff-harness case={mode} phase={phase} feed={}/{} drained={} would_block={}",
            self.fed,
            self.input.len(),
            self.drained,
            self.blocked
        );
    }

    fn drain(&mut self, deadline: Instant) -> std::io::Result<()> {
        let mut bytes = [0; 8192];
        let mut budget = 64 * 1024;
        while budget > 0 && Instant::now() < deadline {
            match self.master.read(&mut bytes[..budget.min(8192)]) {
                Ok(0) => break,
                Ok(count) => {
                    self.drained += count;
                    budget -= count;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(libc::EIO) =>
                {
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub(super) fn until(
        &mut self,
        deadline: Instant,
        feed: bool,
        mut ready: impl FnMut() -> bool,
    ) -> Outcome {
        loop {
            if let Err(error) = self.drain(deadline) {
                return Outcome::IoError(error.kind());
            }
            if ready() {
                return Outcome::Ready;
            }
            if Instant::now() >= deadline {
                return Outcome::Deadline;
            }
            if feed && self.fed < self.input.len() {
                let end = (self.fed + 4096).min(self.input.len());
                match self.master.write(&self.input[self.fed..end]) {
                    Ok(0) => return Outcome::IoError(std::io::ErrorKind::WriteZero),
                    Ok(count) => self.fed += count,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        self.blocked += 1
                    }
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => {} // closed slave: keep observing owner
                    Err(error) => return Outcome::IoError(error.kind()),
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let mut fd = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN
                    | if feed && self.fed < self.input.len() {
                        libc::POLLOUT
                    } else {
                        0
                    },
                revents: 0,
            };
            unsafe { libc::poll(&mut fd, 1, remaining.as_millis().clamp(1, 10) as i32) };
        }
    }
}

fn raw_pty() -> (File, File) {
    let pty = nix::pty::openpty(None, None).unwrap();
    let master = File::from(pty.master);
    let slave = File::from(pty.slave);
    let mut modes = nix::sys::termios::tcgetattr(&slave).unwrap();
    nix::sys::termios::cfmakeraw(&mut modes);
    nix::sys::termios::tcsetattr(&slave, nix::sys::termios::SetArg::TCSANOW, &modes).unwrap();
    nonblocking(&master).unwrap();
    nonblocking(&slave).unwrap();
    (master, slave)
}

fn fill_until_blocked(master: &mut File) {
    let deadline = Instant::now() + Duration::from_secs(1);
    let bytes = [b'x'; 16384];
    loop {
        match master.write(&bytes) {
            Ok(count) => assert!(count > 0),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => panic!("fill owned PTY: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "owned unread PTY must apply backpressure"
        );
    }
}

#[test]
fn blocked_feed_returns_at_deadline_without_a_slave_reader() {
    let (mut master, _slave) = raw_pty();
    fill_until_blocked(&mut master);
    let mut pump = PtyPump::new(master, vec![b'y'; 1024 * 1024]).unwrap();
    let start = Instant::now();
    assert_eq!(
        pump.until(start + Duration::from_millis(40), true, || false),
        Outcome::Deadline
    );
    assert!(pump.blocked > 0);
    assert!(pump.fed < pump.input.len());
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn simultaneous_feed_and_output_drain_make_progress_under_backpressure() {
    let (master, mut slave) = raw_pty();
    let input_size = 256 * 1024;
    let output = vec![b'o'; 256 * 1024];
    let mut pump = PtyPump::new(master, vec![b'i'; input_size]).unwrap();
    let mut consumed = 0;
    let mut produced = 0;
    let mut output_blocked = false;
    let outcome = pump.until(Instant::now() + Duration::from_secs(2), true, || {
        let mut bytes = [0; 8192];
        loop {
            match slave.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => consumed += count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("owned slave read: {error}"),
            }
        }
        while produced < output.len() {
            match slave.write(&output[produced..]) {
                Ok(count) => produced += count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    output_blocked = true;
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("owned slave write: {error}"),
            }
        }
        consumed == input_size && produced == output.len()
    });
    assert_eq!(outcome, Outcome::Ready);
    pump.drain(Instant::now() + Duration::from_secs(1)).unwrap();
    assert!(
        output_blocked,
        "fixture must exercise unread output backpressure"
    );
    assert_eq!(pump.fed, input_size);
    assert_eq!(pump.drained, output.len());
}

#[test]
fn cleanup_drain_continues_without_sending_remaining_input() {
    let (master, mut slave) = raw_pty();
    let mut pump = PtyPump::new(master, vec![b'k'; 17]).unwrap();
    let output = vec![b'c'; 256 * 1024];
    let mut produced = 0;
    let outcome = pump.until(Instant::now() + Duration::from_secs(2), false, || {
        while produced < output.len() {
            match slave.write(&output[produced..]) {
                Ok(count) => {
                    assert!(count > 0);
                    produced += count;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("owned cleanup output: {error}"),
            }
        }
        produced == output.len()
    });
    assert_eq!(outcome, Outcome::Ready);
    pump.drain(Instant::now() + Duration::from_secs(1)).unwrap();
    assert_eq!(pump.fed, 0);
    assert_eq!(pump.drained, output.len());
}

fn child_exited(child: &std::process::Child) -> bool {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        ) == 0
            && info.si_pid() == child.id() as i32
    }
}

struct LegacyWriter {
    child: std::process::Child,
    reaped: bool,
}
impl Drop for LegacyWriter {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let _ = self.child.kill(); // unreaped direct child, no descendants/groups
        let deadline = Instant::now() + Duration::from_secs(1);
        while !child_exited(&self.child) && Instant::now() < deadline {
            unsafe { libc::poll(std::ptr::null_mut(), 0, 2) };
        }
        if child_exited(&self.child) {
            let _ = self.child.wait();
        }
    }
}

#[test]
fn legacy_blocking_feed_outlives_deadline_on_owned_full_pty() {
    let root = tempfile::tempdir().unwrap();
    let (master, _unread_slave) = raw_pty();
    let ready = root.path().join("would-block");
    let mut child = LegacyWriter {
        child: std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process::foreground_handoff_tests::pty_pump::legacy_feed_worker",
                "--nocapture",
            ])
            .env_clear()
            .env("DSH_LEGACY_FEED_READY", &ready)
            .stdin(std::process::Stdio::from(master))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
        reaped: false,
    };
    let ready_deadline = Instant::now() + Duration::from_secs(2);
    while !ready.exists() && !child_exited(&child.child) && Instant::now() < ready_deadline {
        unsafe { libc::poll(std::ptr::null_mut(), 0, 2) };
    }
    assert_eq!(std::fs::read(&ready).unwrap(), b"would-block");
    let deadline = Instant::now() + Duration::from_millis(40);
    while Instant::now() < deadline {
        assert!(
            !child_exited(&child.child),
            "legacy blocking write unexpectedly completed"
        );
        unsafe { libc::poll(std::ptr::null_mut(), 0, 2) };
    }
    child.child.kill().unwrap();
    let reap_deadline = Instant::now() + Duration::from_secs(1);
    while !child_exited(&child.child) && Instant::now() < reap_deadline {
        unsafe { libc::poll(std::ptr::null_mut(), 0, 2) };
    }
    assert!(child_exited(&child.child));
    let _ = child.child.wait().unwrap();
    child.reaped = true;
    let _ = writeln!(
        std::io::stderr().lock(),
        "handoff-harness legacy blocking write: WouldBlock observed, exceeded40ms, owned writer killed/reaped"
    );
}

#[test]
fn legacy_feed_worker() {
    let Some(ready) = std::env::var_os("DSH_LEGACY_FEED_READY") else {
        return;
    };
    let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(0) }
        .try_clone_to_owned()
        .unwrap();
    let mut master = File::from(fd);
    nonblocking(&master).unwrap();
    fill_until_blocked(&mut master);
    std::fs::write(ready, b"would-block").unwrap();
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) },
        0
    );
    // This reproduces the removed source pattern. Its owned parent enforces
    // the deadline externally and reaps this writer, so the suite cannot hang.
    master.write_all(&vec![b'z'; 1024 * 1024]).unwrap();
}
