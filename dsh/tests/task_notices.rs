//! Background task notices on a scratch controlling PTY and local fake provider.
//! No external model requests, inherited credentials or developer terminal.
mod common;

use nix::pty::{Winsize, openpty};
use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Session {
    child: Child,
    master: File,
    output: String,
}
impl Session {
    fn new(root: &tempfile::TempDir, endpoint: &str) -> Self {
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_dogesh"));
        command
            .env_clear()
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
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
            let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
            assert!(flags >= 0);
            assert!(libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0);
        }
        Self {
            child: command.spawn().unwrap(),
            master,
            output: String::new(),
        }
    }
    fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
    }
    fn until(&mut self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(12);
        while !self.output.contains(text) && Instant::now() < deadline {
            self.read();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            self.output.contains(text),
            "missing {text:?}: {}",
            self.output
        );
    }
    fn read(&mut self) {
        let mut bytes = [0; 65536];
        loop {
            match self.master.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => self
                    .output
                    .push_str(&String::from_utf8_lossy(&bytes[..count])),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("PTY read failed: {error}: {}", self.output),
            }
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.master.write_all(b"\x03exit\r");
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.child.id() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
        let _ = self.child.wait();
    }
}

#[test]
fn task_notices_preserve_typed_command_on_real_pty() {
    for mode in ["completed", "failed", "approval_waiting"] {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (ready, requested) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(12);
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
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut bytes = [0; 4096];
            loop {
                let count = socket.read(&mut bytes).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&bytes[..count]);
                if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers =
                        String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= header_end + 4 + length {
                        break;
                    }
                }
            }
            ready.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
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
            write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let mut session = Session::new(&root, &endpoint);
        session.until("\x1b]133;A");
        session.send(b"chat_task start --profile read-only -- private-prompt\r");
        session.until(" started; chat_task status/output/cancel");
        requested.recv_timeout(Duration::from_secs(10)).unwrap();
        session.send(b"printf %s KEEP_TYPED > result");
        session.until("result");
        // A prefix inserted at Home establishes a cursor in the middle of the
        // original input, with visible acknowledgement before provider release.
        session.send(b"\x01echo CURSOR_OK; ");
        session.until("CURSOR_OK");
        release.send(()).unwrap();
        let state = mode;
        session.until(&format!("] {state}\r\n"));
        session.send(b"echo CURSOR_AFTER; \r");
        let deadline = Instant::now() + Duration::from_secs(5);
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
        assert_eq!(session.output.matches(&format!("] {state}\r\n")).count(), 1);
        for line in session.output.split("[AI task ").skip(1) {
            let notice = line.split("\r\n").next().unwrap();
            assert!(!notice.contains("private-"));
            assert!(!notice.contains("synthetic-provider-key"));
        }
        server.join().unwrap();
    }
}
