//! Bounded test-only I/O and owned-child supervision, never a developer tty.
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::Child;
use std::time::{Duration, Instant};

pub const OUTPUT_LIMIT: usize = 256 * 1024;
pub const READ_BUDGET: usize = 16 * 1024;

pub fn phase(mode: &str, stage: &str) {
    // Direct stderr also remains visible when libtest captures eprintln!.
    let _ = writeln!(io::stderr(), "task-notices [{mode}] {stage}");
}

pub fn nonblocking(file: &impl AsRawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn pump(reader: &mut impl Read, output: &mut Vec<u8>) -> io::Result<()> {
    // Exactly one read: a continuously ready stream cannot trap the caller
    // inside this function and defeat its case/phase deadline.
    let mut bytes = [0; READ_BUDGET];
    match reader.read(&mut bytes) {
        Ok(count) => {
            if output.len().saturating_add(count) > OUTPUT_LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "assertion stream exceeded limit; refusing to discard earlier evidence",
                ));
            }
            output.extend_from_slice(&bytes[..count]);
            Ok(())
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub fn write_until(writer: &mut impl Write, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match writer.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub fn wait_until(
    child: &mut Child,
    deadline: Instant,
    mut drain: impl FnMut(),
) -> io::Result<bool> {
    loop {
        drain();
        if child.try_wait()?.is_some() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn kill_owned(child: &mut Child, drain: impl FnMut()) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    // Signal a group only when this still-unreaped direct child owns it.
    // Before ownership ACK/setsid it can share our group, which we never kill.
    let pid = nix::unistd::Pid::from_raw(child.id() as i32);
    if nix::unistd::getpgid(Some(pid)).ok() == Some(pid)
        && let Err(error) = nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGKILL)
    {
        phase(
            "cleanup",
            &format!("owned group kill: {error}; direct fallback"),
        );
    }
    if child.try_wait()?.is_none() {
        child.kill()?;
    }
    if !wait_until(child, Instant::now() + Duration::from_secs(2), drain)? {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "owned child did not reap after kill",
        ));
    }
    Ok(())
}

/// One wait/signal owner; an unreaped Child pins its PID throughout cleanup.
pub struct OwnedChild(pub Child);
impl std::ops::Deref for OwnedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Err(error) = kill_owned(&mut self.0, || {}) {
            phase("cleanup", &format!("owned child cleanup: {error}"));
        }
    }
}

pub fn phase_deadline(case: Instant, budget: Duration) -> Instant {
    case.min(Instant::now() + budget)
}

#[test]
fn pump_returns_with_a_byte_budget_even_when_reader_stays_ready() {
    let mut reader = io::Cursor::new(vec![b'x'; OUTPUT_LIMIT * 2]);
    let mut output = Vec::new();
    pump(&mut reader, &mut output).unwrap();
    assert!(reader.position() as usize <= READ_BUDGET);
    assert_eq!(output.len(), READ_BUDGET);
}

#[test]
fn write_deadline_handles_an_always_blocked_writer() {
    struct Blocked;
    impl Write for Blocked {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::WouldBlock.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let started = Instant::now();
    assert_eq!(
        write_until(
            &mut Blocked,
            b"pending",
            started + Duration::from_millis(30)
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::TimedOut
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn deadline_kills_and_reaps_only_the_direct_owned_child() {
    use std::process::{Command, Stdio};
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "task_notices_stalled_worker", "--nocapture"])
        .env("DOGESH_NOTICE_STALL", "1")
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = OwnedChild(command.spawn().unwrap());
    let started = Instant::now();
    assert!(!wait_until(&mut child, started + Duration::from_millis(150), || {}).unwrap());
    kill_owned(&mut child, || {}).unwrap();
    let status = child.try_wait().unwrap().unwrap();
    assert!(!status.success());
    // Repeated cleanup of a reaped Child cannot signal a reused bare PID.
    kill_owned(&mut child, || {}).unwrap();
    assert_eq!(child.try_wait().unwrap(), Some(status));
    assert!(started.elapsed() < Duration::from_secs(4));
}

/// Read one finite HTTP request on a nonblocking fixture connection.
pub fn read_request(reader: &mut impl Read, deadline: Instant) -> io::Result<()> {
    const LIMIT: usize = 2 * 1024 * 1024;
    let mut request = Vec::new();
    let mut bytes = [0; 4096];
    loop {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match reader.read(&mut bytes) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => request.extend_from_slice(&bytes[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(error) => return Err(error),
        }
        if request.len() > LIMIT {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|length| *length <= LIMIT)
                .ok_or(io::ErrorKind::InvalidData)?;
            if request.len() >= end + 4 + length {
                return Ok(());
            }
        }
    }
}

#[test]
fn provider_request_has_a_total_deadline_and_size_bound() {
    struct Stalled;
    impl Read for Stalled {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
    let started = Instant::now();
    assert_eq!(
        read_request(&mut Stalled, started + Duration::from_millis(30))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    let mut excessive = io::Cursor::new(b"POST / HTTP/1.1\r\nContent-Length: 2097153\r\n\r\n");
    assert_eq!(
        read_request(&mut excessive, Instant::now() + Duration::from_secs(1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn assertion_stream_overflow_fails_without_erasing_early_evidence() {
    let early = b"[AI task duplicate] private-violation\n";
    let mut output = early.to_vec();
    let mut reader = io::Cursor::new(vec![b'x'; OUTPUT_LIMIT * 2]);
    let mut overflow = None;
    for _ in 0..40 {
        if let Err(error) = pump(&mut reader, &mut output) {
            overflow = Some(error);
            break;
        }
    }
    assert_eq!(
        overflow
            .expect("assertion stream must fail on overflow")
            .kind(),
        io::ErrorKind::FileTooLarge
    );
    assert!(output.starts_with(early));
    assert!(output.len() <= OUTPUT_LIMIT);
}
