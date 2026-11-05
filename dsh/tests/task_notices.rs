//! Background task notices on a scratch controlling PTY and local fake provider.
//! No external model requests, inherited credentials or developer terminal.
mod common;
#[path = "task_notices/escaped_continuation.rs"]
mod escaped_continuation;
#[path = "task_notices/support.rs"]
mod support;

use nix::pty::{Winsize, openpty};
use std::fs::File;

use std::net::TcpListener;

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Session {
    child: support::OwnedChild,
    master: File,
    output: Vec<u8>,
    reaped: bool,
    deadline: Instant,
}
impl Session {
    fn new(root: &tempfile::TempDir, endpoint: &str, deadline: Instant) -> Self {
        let pty = openpty(
            Some(&Winsize {
                ws_row: 40,
                ws_col: 100,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )
        .unwrap();
        let master = File::from(pty.master);
        let slave = File::from(pty.slave);
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "task_notices_terminal_worker",
            "--nocapture",
            "--test-threads=1",
        ]);
        command
            .env_clear()
            .env("DOGESH_NOTICE_TERMINAL", "1")
            .envs(common::process::contract_env(root))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TERM", "xterm")
            .env("AI_CHAT_API_KEY", "synthetic-provider-key")
            .env("AI_CHAT_MODEL", "mock-model")
            .env("AI_CHAT_BASE_URL", endpoint)
            .env("AI_CHAT_API", "chat_completions")
            .env("AI_CHAT_ALLOW_INSECURE_HTTP", "1")
            .env("AI_CHAT_SKILL_REFLECT", "0")
            .current_dir(root.path())
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        support::nonblocking(&master).unwrap();
        support::phase("session", "spawning post-exec terminal worker");
        let child = support::OwnedChild(command.spawn().unwrap());
        support::phase("session", &format!("spawned PID {}", child.id()));
        let mut session = Self {
            child,
            master,
            output: Vec::new(),
            reaped: false,
            deadline,
        };
        // The worker cannot isolate until the direct Child owner exists.
        session.send(b"OWNED\n");
        session
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }

    fn send(&mut self, bytes: &[u8]) {
        support::write_until(
            &mut self.master,
            bytes,
            support::phase_deadline(self.deadline, Duration::from_secs(2)),
        )
        .unwrap();
    }
    fn wait_for(&mut self, text: &str) -> bool {
        let deadline = support::phase_deadline(self.deadline, Duration::from_secs(12));
        while !self.text().contains(text) && Instant::now() < deadline {
            self.read();
            std::thread::sleep(Duration::from_millis(10));
        }
        self.text().contains(text)
    }
    fn until(&mut self, text: &str) {
        assert!(self.wait_for(text), "missing {text:?}: {}", self.text());
    }
    fn read(&mut self) {
        if let Err(error) = support::pump(&mut self.master, &mut self.output) {
            // Linux uses EIO and Darwin may revoke the slave at leader exit.
            if error.raw_os_error() != Some(libc::EIO) {
                panic!("PTY read: {error}: {:?}", self.text());
            }
        }
    }
    fn finish(&mut self) -> std::io::Result<()> {
        if self.reaped {
            return Ok(());
        }
        support::phase("session", "draining graceful exit");
        support::write_until(
            &mut self.master,
            b"\x03exit\r",
            Instant::now() + Duration::from_millis(100),
        )
        .ok();
        let (master, output) = (&mut self.master, &mut self.output);
        let mut drain_error = None;
        let mut drain = || {
            if let Err(error) = support::pump(master, output)
                && error.raw_os_error() != Some(libc::EIO)
            {
                drain_error.get_or_insert(error);
            }
        };
        self.reaped = support::wait_until(
            &mut self.child,
            Instant::now() + Duration::from_secs(2),
            &mut drain,
        )?;
        if !self.reaped {
            support::phase("session", "killing owned terminal child");
            support::kill_owned(&mut self.child, &mut drain)?;
            self.reaped = true;
        }
        // Even late output overflow must fail a passing fixture. Reap first,
        // then report the capture error; Drop never strands an owned child.
        if let Some(error) = drain_error {
            return Err(error);
        }
        support::phase("session", "reaped");
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            support::phase("cleanup", &format!("{error}"));
        }
    }
}

#[test]
fn task_notices_terminal_worker() {
    let Ok(mode) = std::env::var("DOGESH_NOTICE_TERMINAL") else {
        return;
    };
    support::phase("session", "waiting ownership ACK before isolation");
    let mut ack = [0; 6];
    std::io::stdin().read_exact(&mut ack).unwrap();
    assert_eq!(&ack, b"OWNED\n");
    support::phase("session", "ownership ACK received");
    if mode == "probe" {
        return;
    }
    support::phase("session", "post-exec setsid/TIOCSCTTY");
    unsafe {
        assert!(libc::setsid() >= 0, "{}", std::io::Error::last_os_error());
        assert_eq!(
            libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0),
            0,
            "{}",
            std::io::Error::last_os_error()
        );
    }
    support::phase("session", "exec dogesh");
    panic!(
        "exec dogesh: {}",
        Command::new(env!("CARGO_BIN_EXE_dogesh")).exec()
    );
}

#[test]
fn task_notices_stalled_worker() {
    if std::env::var_os("DOGESH_NOTICE_STALL").is_none() {
        return;
    }
    support::phase("watchdog-test", "deliberately stalled");
    loop {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn task_notices_preserve_typed_command_on_real_pty() {
    for mode in ["completed", "failed", "approval_waiting"] {
        run_case(mode);
    }
}

fn run_case(mode: &str) {
    let case_deadline = Instant::now() + Duration::from_secs(30);
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (ready, requested) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let mode_owned = mode.to_owned();
    let mut session = Session::new(&root, &endpoint, case_deadline);
    let server = std::thread::spawn(move || {
        let mode = mode_owned;
        support::phase(&mode, "mock accepting");
        let deadline = support::phase_deadline(case_deadline, Duration::from_secs(12));
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("mock provider accept: {error}"),
            }
        };
        support::phase(&mode, "mock reading request");
        socket.set_nonblocking(true).unwrap();
        support::read_request(
            &mut socket,
            support::phase_deadline(case_deadline, Duration::from_secs(5)),
        )
        .unwrap();
        ready.send(()).unwrap();
        support::phase(&mode, "mock waiting release");
        released
            .recv_timeout(
                support::phase_deadline(case_deadline, Duration::from_secs(5))
                    .saturating_duration_since(Instant::now()),
            )
            .unwrap();
        let (status, body) = if mode == "completed" {
            (
                "200 OK",
                r#"{"choices":[{"message":{"role":"assistant","content":"private-worker-output"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
            )
        } else if mode == "approval_waiting" {
            (
                "200 OK",
                r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"execute","arguments":"{\"command\":\"printf private-operation\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
            )
        } else {
            (
                "400 Bad Request",
                r#"{"error":{"message":"private-worker-error","type":"invalid_request_error"}}"#,
            )
        };
        support::phase(&mode, "mock writing response");
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        support::write_until(
            &mut socket,
            response.as_bytes(),
            support::phase_deadline(case_deadline, Duration::from_secs(5)),
        )
        .unwrap();
    });
    support::phase(mode, "waiting initial prompt");
    session.until("\x1b]133;A");
    support::phase(mode, "starting task");
    session.send(b"chat_task start --profile read-only -- private-prompt\r");
    session.until(" started; chat_task status/output/cancel");
    support::phase(mode, "waiting provider request");
    requested
        .recv_timeout(
            support::phase_deadline(case_deadline, Duration::from_secs(10))
                .saturating_duration_since(Instant::now()),
        )
        .unwrap();
    support::phase(mode, "typing input");
    session.send(b"printf %s KEEP_TYPED > result");
    session.until("result");
    // A prefix inserted at Home establishes a cursor in the middle of the
    // original input, with visible acknowledgement before provider release.
    support::phase(mode, "moving cursor and inserting prefix");
    session.send(b"\x01echo CURSOR_OK; ");
    session.until("CURSOR_OK");
    support::phase(mode, "releasing provider");
    release.send(()).unwrap();
    let state = mode;
    support::phase(mode, "waiting notice");
    session.until(&format!("] {state}\r\n"));
    support::phase(mode, "inserting at preserved cursor");
    session.send(b"echo CURSOR_AFTER; \r");
    support::phase(mode, "waiting result file");
    let deadline = support::phase_deadline(case_deadline, Duration::from_secs(5));
    while std::fs::read(root.path().join("result")).ok().as_deref() != Some(b"KEEP_TYPED")
        && Instant::now() < deadline
    {
        session.read();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read(root.path().join("result")).unwrap(),
        b"KEEP_TYPED"
    );
    // Another command/tick must not repeat the completed task's notice.
    session.send(b"echo NOTICE_CHECK\r");
    session.until("NOTICE_CHECK\r\n");
    assert_eq!(session.text().matches(&format!("] {state}\r\n")).count(), 1);
    for line in session.text().split("[AI task ").skip(1) {
        let notice = line.split("\r\n").next().unwrap();
        assert!(!notice.contains("private-"));
        assert!(!notice.contains("synthetic-provider-key"));
    }
    support::phase(mode, "waiting provider thread exit");
    let deadline = support::phase_deadline(case_deadline, Duration::from_secs(5));
    while !server.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(server.is_finished(), "provider thread deadline");
    server.join().unwrap();
    session.finish().unwrap();
    support::phase(mode, "scenario finished");
}

#[test]
fn task_notices_terminal_waits_for_ownership_before_isolation() {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "task_notices_terminal_worker", "--nocapture"])
        .env("DOGESH_NOTICE_TERMINAL", "probe")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = support::OwnedChild(command.spawn().unwrap());
    let mut stderr = child.stderr.take().unwrap();
    support::nonblocking(&stderr).unwrap();
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !String::from_utf8_lossy(&output).contains("waiting ownership ACK")
        && Instant::now() < deadline
    {
        support::pump(&mut stderr, &mut output).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(String::from_utf8_lossy(&output).contains("waiting ownership ACK"));
    let pid = nix::unistd::Pid::from_raw(child.id() as i32);
    assert_eq!(
        nix::unistd::getpgid(Some(pid)).unwrap(),
        nix::unistd::getpgrp()
    );
    assert_eq!(
        nix::unistd::getsid(Some(pid)).unwrap(),
        nix::unistd::getsid(None).unwrap()
    );
    let mut stdin = child.stdin.take().unwrap();
    support::nonblocking(&stdin).unwrap();
    support::write_until(&mut stdin, b"OWNED\n", deadline).unwrap();
    drop(stdin);
    assert!(
        support::wait_until(&mut child, deadline, || {
            support::pump(&mut stderr, &mut output).unwrap();
        })
        .unwrap()
    );
    support::pump(&mut stderr, &mut output).unwrap();
    assert!(child.try_wait().unwrap().unwrap().success());
    assert!(String::from_utf8_lossy(&output).contains("ownership ACK received"));
}

#[test]
fn task_notices_total_case_deadline_bounds_a_missing_phase_and_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::new(
        &root,
        "http://127.0.0.1:1",
        Instant::now() + Duration::from_secs(30),
    );
    session.until("\x1b]133;A");
    let started = Instant::now();
    session.deadline = started + Duration::from_millis(50);
    assert!(!session.wait_for("deliberately missing notice"));
    assert!(started.elapsed() < Duration::from_secs(2));
    session.finish().unwrap();
    assert!(session.child.try_wait().unwrap().is_some());
}
