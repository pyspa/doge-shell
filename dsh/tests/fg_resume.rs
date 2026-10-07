//! Foreground continuation on a scratch controlling PTY, never the user's tty.
mod common;

use nix::pty::{Winsize, openpty};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, getpgid, getsid};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct Session {
    child: Child,
    master: File,
    root: TempDir,
    output: Vec<u8>,
    groups: Vec<Pid>,
    fixtures: Vec<(Pid, Pid, Pid)>,
}

impl Session {
    fn new() -> Self {
        Self::with_no_pty(false)
    }

    fn with_no_pty(no_pty: bool) -> Self {
        let root = TempDir::new().unwrap();
        let pty = openpty(
            Some(&Winsize {
                ws_row: 40,
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
        cmd.current_dir(root.path())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .envs(common::process::contract_env(&root))
            .env("TERM", "xterm")
            .env("SAFETY_LEVEL", "loose")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        if no_pty {
            cmd.env("DOGESH_NO_PTY", "1");
        }
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
        let child = cmd.spawn().unwrap();
        let mut session = Self {
            child,
            master,
            root,
            output: vec![],
            groups: vec![],
            fixtures: vec![],
        };
        session.until(|s| s.contains(0, b"\x1b]133;B"));
        session
    }

    fn write_fixture(&self, name: &str, body: &str) {
        fs::write(self.root.path().join(name), body).unwrap();
    }

    fn contains(&self, from: usize, bytes: &[u8]) -> bool {
        self.output[from..].windows(bytes.len()).any(|w| w == bytes)
    }

    fn pump(&mut self, timeout: Duration) {
        let mut fd = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe {
            libc::poll(&mut fd, 1, timeout.as_millis().min(20) as i32);
        }
        let mut buf = [0; 8192];
        loop {
            match self.master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => self.output.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        // Each PID comes from an isolated fixture. FullProxy deliberately
        // creates its own session; that fixture also records its shell parent.
        let shell = Pid::from_raw(self.child.id() as i32);
        for name in ["left.pid", "right.pid", "child.pid"] {
            if let Ok(text) = fs::read_to_string(self.root.path().join(name))
                && let Ok(raw) = text.trim().parse::<i32>()
            {
                let pid = Pid::from_raw(raw);
                if let (Ok(group), Ok(session)) = (getpgid(Some(pid)), getsid(Some(pid))) {
                    let full_proxy = name == "child.pid"
                        && session == pid
                        && group == pid
                        && fs::read_to_string(self.root.path().join("child.parent"))
                            .ok()
                            .and_then(|p| p.trim().parse::<i32>().ok())
                            == Some(shell.as_raw());
                    if group.as_raw() > 0 && group != shell && (session == shell || full_proxy) {
                        if !self.groups.contains(&group) {
                            self.groups.push(group);
                        }
                        let fixture = (pid, session, group);
                        if !self.fixtures.contains(&fixture) {
                            self.fixtures.push(fixture);
                        }
                    }
                }
            }
        }
    }

    fn until(&mut self, predicate: impl Fn(&Self) -> bool) {
        let end = Instant::now() + Duration::from_secs(8);
        while !predicate(self) && Instant::now() < end {
            self.pump(Duration::from_millis(20));
        }
        assert!(
            predicate(self),
            "PTY deadline: {}",
            String::from_utf8_lossy(&self.output)
        );
    }

    fn send(&mut self, command: &str) -> usize {
        let offset = self.output.len();
        self.master.write_all(command.as_bytes()).unwrap();
        self.master.write_all(b"\r").unwrap();
        offset
    }

    fn stopped(&mut self, command: &str) {
        let offset = self.send(command);
        self.until(|s| s.contains(offset, b"Stopped") && s.contains(offset, b"\x1b]133;B"));
    }

    fn finished(&mut self, offset: usize, status: i32) {
        let marker = format!("\x1b]133;D;{status}");
        self.until(|s| s.contains(offset, marker.as_bytes()) && s.contains(offset, b"\x1b]133;B"));
        // A successful bg command can return while its pipeline is live.
        // Keep those owned fixtures available for failure-path cleanup.
        self.fixtures.retain(|(pid, session, group)| {
            getsid(Some(*pid)) == Ok(*session) && getpgid(Some(*pid)) == Ok(*group)
        });
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // These groups were discovered solely from this session's fixture PID files.
        for (pid, session, group) in &self.fixtures {
            if getsid(Some(*pid)) == Ok(*session) && getpgid(Some(*pid)) == Ok(*group) {
                let _ = killpg(*group, Signal::SIGKILL);
            }
        }
        let _ = killpg(Pid::from_raw(self.child.id() as i32), Signal::SIGKILL);
        let _ = self.child.wait();
    }
}

#[test]
fn full_proxy_resume_restops_and_restores_input() {
    let _serial = common::serial_guard();
    let mut s = Session::new();
    s.write_fixture("stop.sh", "echo $$ > child.pid\necho $PPID > child.parent\nkill -STOP $$\necho ready > ready\nread value\necho \"$value\" > input\nkill -STOP $$\nexit 7\n");
    s.stopped("sh stop.sh");
    assert_eq!(
        s.fixtures.len(),
        1,
        "FullProxy fixture must be owned for failure cleanup"
    );
    let first = s.send("fg");
    s.until(|s| s.root.path().join("ready").exists());
    s.send("proxy-input");
    s.until(|s| s.contains(first, b"Stopped") && s.contains(first, b"\x1b]133;B"));
    assert_eq!(
        fs::read_to_string(s.root.path().join("input"))
            .unwrap()
            .trim(),
        "proxy-input"
    );
    let second = s.send("fg");
    s.finished(second, 7);
    let echo = s.send("echo prompt-ready");
    s.finished(echo, 0);
    assert!(s.contains(echo, b"prompt-ready"));
}

#[test]
fn output_only_pipeline_resumes_entire_group_and_waits_for_tail() {
    let _serial = common::serial_guard();
    output_pipeline_resume(false, false);
}

#[test]
fn no_pty_pipeline_resumes_entire_group_and_waits_for_tail() {
    let _serial = common::serial_guard();
    output_pipeline_resume(true, false);
}

#[test]
fn no_pty_background_resume_keeps_pipeline_group() {
    let _serial = common::serial_guard();
    output_pipeline_resume(true, true);
}

fn output_pipeline_resume(no_pty: bool, background: bool) {
    let mut s = Session::with_no_pty(no_pty);
    s.write_fixture(
        "left.sh",
        "echo $$ > left.pid\nkill -STOP $$\necho payload\nexit 3\n",
    );
    s.write_fixture("right.sh", "echo $$ > right.pid\nkill -STOP $$\nread value\necho \"$value\" > received\nread release < gate\nexit 7\n");
    nix::unistd::mkfifo(
        &s.root.path().join("gate"),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    s.stopped("sh left.sh < /dev/null | sh right.sh");
    assert_eq!(s.groups.len(), 1, "pipeline must share one owned group");
    if background {
        let bg = s.send("bg");
        s.finished(bg, 0);
        s.until(|s| s.root.path().join("received").exists());
    }
    let fg = s.send("fg");
    s.until(|s| s.root.path().join("received").exists());
    let left = fs::read_to_string(s.root.path().join("left.pid"))
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    s.until(|_| getpgid(Some(Pid::from_raw(left))).is_err());
    assert!(
        !s.contains(fg, b"\x1b]133;D;"),
        "fg returned while resumed tail was running"
    );
    use std::os::unix::fs::OpenOptionsExt;
    let end = Instant::now() + Duration::from_secs(8);
    let mut gate = loop {
        match fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(s.root.path().join("gate"))
        {
            Ok(file) => break file,
            Err(e) if e.raw_os_error() == Some(libc::ENXIO) && Instant::now() < end => {
                s.pump(Duration::from_millis(20))
            }
            Err(e) => panic!("open release FIFO: {e}"),
        }
    };
    gate.write_all(b"release\n").unwrap();
    s.finished(fg, 7);
    assert_eq!(
        fs::read_to_string(s.root.path().join("received"))
            .unwrap()
            .trim(),
        "payload"
    );
}

#[test]
fn completed_stage_survives_foreground_resume() {
    let _serial = common::serial_guard();
    let mut s = Session::new();
    s.write_fixture("left.sh", "exit 3\n");
    s.write_fixture("right.sh", "echo $$ > right.pid\nkill -STOP $$\nexit 7\n");
    s.stopped("sh left.sh < /dev/null | sh right.sh");
    let fg = s.send("fg");
    s.finished(fg, 7);
}

#[test]
fn no_pty_external_preserves_exit_status_after_exec() {
    let _serial = common::serial_guard();
    let mut s = Session::with_no_pty(true);
    s.write_fixture("exit.sh", "echo launched-no-pty\nexit 7\n");
    let offset = s.send("sh exit.sh");
    s.finished(offset, 7);
    assert!(s.contains(offset, b"launched-no-pty"));
    assert!(!s.contains(offset, b"EACCES"));
    let missing = s.send("missing_no_pty_command");
    s.finished(missing, 127);
    s.write_fixture("not-executable", "exit 99\n");
    let denied = s.send("./not-executable");
    // Preserve the existing exec-failure policy; do not turn it into success.
    s.finished(denied, 1);
    assert!(s.contains(denied, b"Permission denied"));
    let redirect = s.send("sh exit.sh < missing-no-pty-input");
    s.finished(redirect, 1);
    let echo = s.send("echo prompt-ready");
    s.finished(echo, 0);
}

#[test]
fn no_pty_terminal_sigint_reaches_entire_pipeline() {
    let _serial = common::serial_guard();
    let mut s = Session::with_no_pty(true);
    for name in ["left", "right"] {
        let fixture_binary = std::env::current_exe().unwrap();
        let fixture_binary = fixture_binary.to_str().unwrap().replace('\'', "'\"'\"'");
        s.write_fixture(
            &format!("{name}.sh"),
            &format!("DOGESH_TEST_SIGNAL_FIXTURE={name} exec '{fixture_binary}' --exact no_pty_signal_fixture --nocapture\n"),
        );
    }
    let offset = s.send("sh left.sh < /dev/null | sh right.sh");
    s.until(|s| {
        s.root.path().join("left.ready").exists()
            && s.root.path().join("right.ready").exists()
            && s.fixtures.len() == 2
    });
    assert_eq!(s.groups.len(), 1);
    let group = s.groups[0];
    assert_ne!(group.as_raw(), s.child.id() as i32);
    s.master.write_all(&[3]).unwrap();
    s.finished(offset, 130);
    for name in ["left", "right"] {
        let pid = fs::read_to_string(s.root.path().join(format!("{name}.pid")))
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        assert!(
            getpgid(Some(Pid::from_raw(pid))).is_err(),
            "interrupted stage must be reaped"
        );
    }
    let echo = s.send("echo survived-interrupt");
    s.finished(echo, 0);
}

// Re-exec only inside the scratch shell's session: querying the PTY master
// from the parent is outside macOS's controlling-terminal guarantee.
#[test]
fn no_pty_signal_fixture() {
    let Ok(name) = std::env::var("DOGESH_TEST_SIGNAL_FIXTURE") else {
        return;
    };
    assert!(matches!(name.as_str(), "left" | "right"));
    fs::write(format!("{name}.pid"), nix::unistd::getpid().to_string()).unwrap();
    // The shell may capture stderr through a pipe. /dev/tty resolves only
    // to this fixture's scratch controlling terminal, not the test parent's.
    let terminal = File::open("/dev/tty").unwrap();
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        let foreground = unsafe { libc::tcgetpgrp(terminal.as_raw_fd()) };
        assert!(foreground > 0, "fixture must own a controlling terminal");
        if foreground == nix::unistd::getpgrp().as_raw() {
            break;
        }
        assert!(Instant::now() < end, "fixture never became foreground");
        unsafe { libc::poll(std::ptr::null_mut(), 0, 20) };
    }
    fs::write(format!("{name}.ready"), "foreground").unwrap();
    loop {
        unsafe { libc::pause() };
    }
}
