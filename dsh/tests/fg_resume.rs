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
        for name in ["left.pid", "right.pid", "child.pid", "producer.pid"] {
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
            "PTY deadline: {}\nSpawn diagnostics: {}",
            String::from_utf8_lossy(&self.output),
            fs::read_to_string(self.root.path().join("state/dogesh/debug.log")).unwrap_or_default()
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
        // Auxiliary substitution helpers can still be draining after the
        // foreground tree completed. Keep their live ownership for cleanup.
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
fn failed_exec_reports_126_and_returns_to_interactive_prompt() {
    let _serial = common::serial_guard();
    let mut s = Session::new();
    s.write_fixture("denied", "#!/bin/sh\necho SHOULD-NOT-RUN\n");
    std::fs::set_permissions(
        s.root.path().join("denied"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    let denied = s.send("./denied");
    s.finished(denied, 126);
    let captured = s.send("x=$(./denied); echo CAPTURE:$?");
    s.finished(captured, 0);
    assert!(s.contains(captured, b"CAPTURE:126"));
    let recovered = s.send("echo exec-prompt-ready");
    s.finished(recovered, 0);
}

#[test]
fn denied_path_candidate_displays_diagnostic_on_interactive_terminal() {
    let _serial = common::serial_guard();
    let mut s = Session::new();
    fs::create_dir(s.root.path().join("bin")).unwrap();
    s.write_fixture("bin/doge-path-denied-xyz", "blocked");
    fs::set_permissions(
        s.root.path().join("bin/doge-path-denied-xyz"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    let setup = s.send("PATH=bin:$PATH");
    s.finished(setup, 0);
    for _ in 0..8 {
        let denied = s.send("doge-path-denied-xyz");
        s.finished(denied, 126);
        s.until(|s| s.contains(denied, b"Permission denied"));
        assert!(!s.contains(denied, b"command not found"));
    }
    let recovered = s.send("echo path-prompt-ready");
    s.finished(recovered, 0);
}

#[test]
fn denied_path_candidate_reports_126_and_returns_to_interactive_prompt() {
    let _serial = common::serial_guard();
    let mut s = Session::new();
    fs::create_dir(s.root.path().join("bin")).unwrap();
    s.write_fixture("bin/doge-path-denied-xyz", "blocked");
    fs::set_permissions(
        s.root.path().join("bin/doge-path-denied-xyz"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    let setup = s.send("PATH=bin:$PATH");
    s.finished(setup, 0);
    // Keep terminal status/prompt coverage separate from diagnostic transport.
    // Unredirected diagnostics were missing on macOS CI; investigating that
    // PTY transport path is separate from PATH resolution here.
    let denied = s.send("doge-path-denied-xyz 2>denied.err");
    s.finished(denied, 126);
    let diagnostic = fs::read_to_string(s.root.path().join("denied.err")).unwrap();
    assert!(diagnostic.contains("Permission denied"));
    assert!(!diagnostic.contains("command not found"));
    let missing = s.send("doge-path-missing-xyz 2>missing.err");
    s.finished(missing, 127);
    assert!(
        fs::read_to_string(s.root.path().join("missing.err"))
            .unwrap()
            .contains("command not found")
    );
    let success = s.send("true 2>success.err");
    s.finished(success, 0);
    let pipeline = s.send("echo upstream | doge-path-denied-xyz 2>pipeline.err");
    s.finished(pipeline, 126);
    assert!(
        fs::read_to_string(s.root.path().join("pipeline.err"))
            .unwrap()
            .contains("Permission denied")
    );
    let pipefail = s.send("set -o pipefail; doge-path-denied-xyz 2>head.err | true");
    s.finished(pipefail, 126);
    let recovered = s.send("echo path-prompt-ready");
    s.finished(recovered, 0);
    assert!(s.contains(recovered, b"path-prompt-ready"));
}

#[test]
fn redirected_input_and_failure_preserve_interactive_pipeline() {
    let _serial = common::serial_guard();
    let mut s = Session::new();
    s.write_fixture("input.txt", "PTY-REDIRECT-PAYLOAD\n");
    let copied = s.send("cat < input.txt | cat > copied.txt");
    s.finished(copied, 0);
    assert_eq!(
        fs::read_to_string(s.root.path().join("copied.txt")).unwrap(),
        "PTY-REDIRECT-PAYLOAD\n"
    );
    let failed = s.send("echo unused > copied.txt < missing-input");
    s.finished(failed, 1);
    assert_eq!(fs::read(s.root.path().join("copied.txt")).unwrap(), b"");
    let recovered = s.send("cat < input.txt | cat > recovered.txt");
    s.finished(recovered, 0);
    assert_eq!(
        fs::read_to_string(s.root.path().join("recovered.txt")).unwrap(),
        "PTY-REDIRECT-PAYLOAD\n"
    );
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
    let mut s = Session::new();
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

impl Session {
    fn fifo(&self, name: &str) {
        nix::unistd::mkfifo(
            &self.root.path().join(name),
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
    }

    fn release(&mut self, name: &str) {
        use std::os::unix::fs::OpenOptionsExt;
        let end = Instant::now() + Duration::from_secs(8);
        let mut gate = loop {
            match fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(self.root.path().join(name))
            {
                Ok(file) => break file,
                Err(e) if e.raw_os_error() == Some(libc::ENXIO) && Instant::now() < end => {
                    self.pump(Duration::from_millis(20))
                }
                Err(e) => panic!("release {name}: {e}"),
            }
        };
        gate.write_all(b"release\n").unwrap();
    }
}

#[test]
fn stopped_read_substitution_survives_two_foreground_intervals() {
    let _serial = common::serial_guard();
    let mut s = Session::new();
    s.fifo("started");
    s.fifo("gate");
    s.fifo("linger");
    s.write_fixture("producer.sh", "echo $$ > producer.pid\necho $PPID > helper.pid\necho ready > started\nread release < gate\nprintf retained-input\nread linger < linger\n");
    s.write_fixture("outer.sh", "echo $$ > child.pid\necho $PPID > child.parent\nread ready < started\nkill -STOP $$\necho first > first-resume\nkill -STOP $$\necho second > second-resume\nhead -c 14 \"$1\" > actual\nexit 7\n");
    s.stopped("sh outer.sh <(sh producer.sh)");
    let helper = Pid::from_raw(
        fs::read_to_string(s.root.path().join("helper.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap(),
    );
    assert!(
        getpgid(Some(helper)).is_ok(),
        "stopping the consumer must not reap its Read helper"
    );
    let first = s.send("fg");
    s.until(|s| {
        s.root.path().join("first-resume").exists()
            && s.contains(first, b"Stopped")
            && s.contains(first, b"\x1b]133;B")
    });
    assert!(
        getpgid(Some(helper)).is_ok(),
        "re-stopping must retain the same Read helper"
    );
    let second = s.send("fg");
    s.until(|s| s.root.path().join("second-resume").exists());
    s.release("gate");
    s.finished(second, 7);
    assert_eq!(
        fs::read_to_string(s.root.path().join("actual")).unwrap(),
        "retained-input"
    );
    assert!(
        getpgid(Some(helper)).is_err(),
        "completed foreground consumer must synchronously reap a lingering Read helper"
    );
}

#[test]
fn stopped_write_substitution_drains_to_eof_after_foreground_completion() {
    let _serial = common::serial_guard();
    let mut s = Session::new();
    s.fifo("started");
    s.fifo("after-drain");
    s.write_fixture(
        "consumer.sh",
        "echo $$ > producer.pid\necho ready > started\ncat > actual\necho drained > drained\nread release < after-drain\n",
    );
    s.write_fixture("outer.sh", "echo $$ > child.pid\necho $PPID > child.parent\nread ready < started\nprintf first > \"$1\"\nkill -STOP $$\nprintf second > \"$1\"\nexit 7\n");
    s.stopped("sh outer.sh >(sh consumer.sh)");
    assert!(
        !s.root.path().join("drained").exists(),
        "a stopped writer is not EOF"
    );
    let fg = s.send("fg");
    s.finished(fg, 7);
    s.until(|s| s.root.path().join("drained").exists());
    assert_eq!(
        fs::read_to_string(s.root.path().join("actual")).unwrap(),
        "firstsecond"
    );
    assert!(
        !s.fixtures.is_empty(),
        "a live asynchronous consumer remains owned for failure cleanup"
    );
    s.release("after-drain");
}
