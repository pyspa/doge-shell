//! Actual Alt+s on an owned PTY with empty credentials, config and history.
use nix::pty::{Winsize, openpty};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

struct Session {
    child: Child,
    master: File,
    output: Vec<u8>,
    reaped: bool,
}

impl Session {
    fn drain(&mut self) {
        let mut bytes = [0; 8192];
        // Bound each drain too: an unexpectedly chatty child must not defeat
        // the outer deadline by keeping read() continuously ready.
        for _ in 0..32 {
            match self.master.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => self.output.extend_from_slice(&bytes[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
                Err(e) => panic!("PTY read: {e}"),
            }
        }
    }

    fn feed(&mut self, mut bytes: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !bytes.is_empty() {
            assert!(Instant::now() < deadline, "PTY input deadline");
            self.drain();
            match self.master.write(bytes) {
                Ok(0) => panic!("PTY input closed"),
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("PTY write: {e}"),
            }
        }
    }

    fn until(&mut self, needle: &str, start: usize, bound: Duration) {
        let deadline = Instant::now() + bound;
        loop {
            self.drain();
            if String::from_utf8_lossy(&self.output[start..]).contains(needle) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing {needle:?}: {}",
                String::from_utf8_lossy(&self.output)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.drain();
            if let Some(status) = self.child.try_wait().unwrap() {
                self.reaped = true;
                self.drain();
                assert!(status.success(), "shell exit: {status}");
                return;
            }
            assert!(Instant::now() < deadline, "shell exit deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // Only this still-owned Child; never signal a saved PID/process group.
        self.child.kill().ok();
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            self.drain();
            if self.child.try_wait().ok().flatten().is_some() {
                self.reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn unconfigured_alt_s_notifies_promptly_and_keeps_editing_on_owned_pty() {
    let root = tempfile::tempdir().unwrap();
    let pty = openpty(
        Some(&Winsize {
            ws_row: 30,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }),
        None,
    )
    .unwrap();
    let master = File::from(pty.master);
    let slave = File::from(pty.slave);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dogesh"));
    cmd.env_clear()
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("PATH", root.path())
        .env("TERM", "xterm-256color")
        .env("LANG", "C.UTF-8")
        .current_dir(root.path())
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave);
    // SAFETY: only async-signal-safe syscalls before exec; fd 0 is our slave.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        assert!(flags >= 0);
        assert!(libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0);
    }
    let mut session = Session {
        child: cmd.spawn().unwrap(),
        master,
        output: Vec::new(),
        reaped: false,
    };
    session.until("\x1b[?25h", 0, Duration::from_secs(5));
    session.feed(b"synthetic-typed-draft");
    session.until("synthetic-typed-draft", 0, Duration::from_secs(2));
    let start = session.output.len();
    let elapsed = Instant::now();
    session.feed(b"\x1bs");
    session.until(
        dsh_openai::API_KEY_SETUP_HINT,
        start,
        Duration::from_secs(2),
    );
    session.until("synthetic-typed-draft", start, Duration::from_secs(2));
    let notice = String::from_utf8_lossy(&session.output[start..]);
    assert!(!notice.contains("Generating"));
    assert!(!notice.contains("\x1b]133;A"));
    assert!(notice.contains("\x1b[?25h"));
    eprintln!(
        "unconfigured Alt+s notice and redraw: {:?}",
        elapsed.elapsed()
    );
    session.feed(b"Z");
    session.until("synthetic-typed-draftZ", start, Duration::from_secs(2));
    // Never execute the typed draft or launch an external command/job.
    session.feed(b"\x03");
    let cleared = session.output.len();
    session.until("\x1b]133;B", cleared, Duration::from_secs(2));
    session.feed(b"exit\r");
    session.finish();
}
