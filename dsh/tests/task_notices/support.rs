//! Bounded test-only I/O and owned-child supervision, never a developer tty.
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const TAIL_LIMIT: usize = 256 * 1024;
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
    // inside this function and defeat its phase/watchdog deadline.
    let mut bytes = [0; READ_BUDGET];
    match reader.read(&mut bytes) {
        Ok(count) => {
            let discard = output
                .len()
                .saturating_add(count)
                .saturating_sub(TAIL_LIMIT);
            output.drain(..discard);
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
    // All callers spawned this PID as its own group/session; ESRCH before
    // post-exec setsid is handled by the checked direct-child fallback.
    let group = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
    if let Err(error) = group {
        phase(
            "cleanup",
            &format!("group kill: {error}; direct owned-child fallback"),
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

fn cleanup_registered_terminal(pid_file: &Path) {
    // A private file written only by this scenario registers its owned terminal
    // child. Consume once; the scenario remains that child's sole wait owner.
    if let Ok(record) = std::fs::read_to_string(pid_file) {
        let _ = std::fs::remove_file(pid_file);
        if let Ok(pid) = record.trim().parse::<i32>()
            && pid > 1
        {
            let pid = nix::unistd::Pid::from_raw(pid);
            if let Err(error) = nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGKILL) {
                phase(
                    "watchdog",
                    &format!("registered group kill: {error}; direct fallback"),
                );
                if let Err(error) = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL)
                    && error != nix::errno::Errno::ESRCH
                {
                    phase(
                        "watchdog",
                        &format!("registered terminal kill failed: {error}"),
                    );
                }
            }
        }
    }
}

pub fn supervise(mut command: Command, timeout: Duration, pid_file: &Path) -> io::Result<()> {
    command
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    struct Owned(Child, std::path::PathBuf);
    impl Drop for Owned {
        fn drop(&mut self) {
            cleanup_registered_terminal(&self.1);
            let _ = kill_owned(&mut self.0, || {});
        }
    }
    let mut owned = Owned(command.spawn()?, pid_file.into());
    let child = &mut owned.0;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    let mut err = Vec::new();
    loop {
        pump(&mut stdout, &mut out)?;
        pump(&mut stderr, &mut err)?;
        if let Some(status) = child.try_wait()? {
            pump(&mut stdout, &mut out)?;
            pump(&mut stderr, &mut err)?;
            phase("watchdog", &String::from_utf8_lossy(&err));
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "scenario exited {status}; stdout={:?}; stderr={:?}",
                    String::from_utf8_lossy(&out),
                    String::from_utf8_lossy(&err)
                )))
            };
        }
        if Instant::now() >= deadline {
            cleanup_registered_terminal(pid_file);
            kill_owned(child, || {
                let _ = pump(&mut stdout, &mut out);
                let _ = pump(&mut stderr, &mut err);
            })?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "scenario watchdog; stdout={:?}; stderr={:?}",
                    String::from_utf8_lossy(&out),
                    String::from_utf8_lossy(&err)
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn pump_returns_with_a_byte_budget_even_when_reader_stays_ready() {
    let mut reader = io::Cursor::new(vec![b'x'; TAIL_LIMIT * 2]);
    let mut output = Vec::new();
    pump(&mut reader, &mut output).unwrap();
    assert!(reader.position() as usize <= READ_BUDGET);
    for _ in 0..40 {
        pump(&mut reader, &mut output).unwrap();
    }
    assert_eq!(output.len(), TAIL_LIMIT);
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
fn watchdog_kills_and_reaps_its_unresponsive_owned_child() {
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "task_notices_stalled_worker", "--nocapture"])
        .env("DOGESH_NOTICE_STALL", "1");
    let started = Instant::now();
    let error = supervise(
        command,
        Duration::from_millis(150),
        &root.path().join("pid"),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("deliberately stalled"));
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
