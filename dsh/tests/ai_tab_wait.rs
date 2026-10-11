//! Delayed AI requests on an owned controlling PTY; no API or user terminal.
mod common;

use anyhow::Result;
use async_trait::async_trait;
use crossterm::event::{Event, EventStream, KeyCode};
use doge_shell::ai_features::{
    AiService, await_with_progress, expand_smart_pipe, run_generative_command,
};
use futures::StreamExt;
use nix::pty::openpty;
use serde_json::{Value, json};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

#[derive(Default)]
struct DelayedService {
    requests: AtomicUsize,
    cancels: AtomicUsize,
    release: Arc<Notify>,
    completed: Arc<Notify>,
    stale_dropped: Arc<AtomicUsize>,
}

#[async_trait]
impl AiService for DelayedService {
    fn cancel_requests(&self) {
        self.cancels.fetch_add(1, Ordering::SeqCst);
    }

    async fn send_request(
        &self,
        _messages: Vec<Value>,
        _temperature: Option<f64>,
    ) -> Result<String> {
        let id = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        println!("REQUEST:{id}");
        std::io::stdout().flush().unwrap();
        if id == 1 {
            // The provider can finish after its consumer was dropped. Keep a
            // separate task and completion notification to exercise that race.
            let (tx, rx) = tokio::sync::oneshot::channel();
            let release = self.release.clone();
            let completed = self.completed.clone();
            let stale_dropped = self.stale_dropped.clone();
            tokio::spawn(async move {
                release.notified().await;
                if tx
                    .send(json!({"command":"echo", "args":["stale"]}).to_string())
                    .is_err()
                {
                    stale_dropped.fetch_add(1, Ordering::SeqCst);
                }
                completed.notify_one();
            });
            Ok(rx.await?)
        } else {
            Ok(json!({"command":"echo", "args":["retry"]}).to_string())
        }
    }
}

async fn request(service: &DelayedService, pipe: bool) -> Result<String> {
    if pipe {
        expand_smart_pipe(service, "日本語 query").await
    } else {
        run_generative_command(service, "日本語 query").await
    }
}

#[tokio::test(flavor = "current_thread")]
async fn owned_pty_worker() {
    let Ok(mode) = std::env::var("DOGESH_TEST_AI_WAIT") else {
        return;
    };
    let pipe = std::env::var("DOGESH_TEST_AI_PIPE").unwrap() == "true";
    let service = DelayedService::default();
    if mode == "plain" {
        service.release.notify_one();
        let answer = await_with_progress("AI Tab", &service, request(&service, pipe)).await;
        assert_eq!(
            answer.unwrap().unwrap().replace("\"echo\"", "echo"),
            "echo stale"
        );
        assert_eq!(service.cancels.load(Ordering::SeqCst), 0);
        println!("PLAIN_DONE");
        return;
    }
    crossterm::terminal::enable_raw_mode().unwrap();
    // The REPL retains its reader while a key handler awaits the temporary
    // progress reader. Poll it for Tab first, then leave it idle during AI.
    let mut repl_keys = EventStream::new();
    println!("READER_READY");
    std::io::stdout().flush().unwrap();
    assert!(
        matches!(repl_keys.next().await.unwrap().unwrap(), Event::Key(key) if key.code == KeyCode::Tab)
    );
    if mode == "direct" {
        // Bounded reproduction of the former REPL direct-await path: Esc/
        // Ctrl-C are not consumed while the provider future is pending.
        assert!(
            tokio::time::timeout(Duration::from_millis(400), request(&service, pipe))
                .await
                .is_err()
        );
        assert_eq!(service.cancels.load(Ordering::SeqCst), 0);
        service.release.notify_one();
        service.completed.notified().await;
        println!("DIRECT_PENDING");
    } else {
        let original = if pipe {
            "cat payload |? 日本語 query"
        } else {
            "?? 日本語 query"
        };
        let mut input = original.to_string();
        let answer = await_with_progress("AI Tab", &service, request(&service, pipe)).await;
        assert!(answer.is_none());
        assert_eq!(service.cancels.load(Ordering::SeqCst), 1);
        println!("CANCELLED");
        std::io::stdout().flush().unwrap();
        assert!(
            matches!(repl_keys.next().await.unwrap().unwrap(), Event::Key(key) if key.code == KeyCode::Char('r'))
        );
        println!("READER_RESUMED");
        service.release.notify_one();
        service.completed.notified().await;
        assert_eq!(
            service.stale_dropped.load(Ordering::SeqCst),
            1,
            "retired wait must drop its receiver before a late reply"
        );
        assert_eq!(input, original, "late response must not replace the query");
        println!("LATE_RETIRED");
        let answer = await_with_progress("AI Tab", &service, request(&service, pipe))
            .await
            .unwrap()
            .unwrap();
        // Quoting is the independent literal-argv renderer contract.
        assert_eq!(answer.replace("\"echo\"", "echo"), "echo retry");
        input = if pipe {
            format!("cat payload | {answer}")
        } else {
            answer
        };
        assert!(input.replace("\"echo\"", "echo").ends_with("echo retry"));
        assert_eq!(service.requests.load(Ordering::SeqCst), 2);
        assert_eq!(service.cancels.load(Ordering::SeqCst), 1);
        println!("RETRY_DONE");
    }
    crossterm::terminal::disable_raw_mode().unwrap();
    // Inspect before this owned session leader exits: Darwin revokes the
    // slave at leader exit, so tcgetattr after child.wait is too late.
    drop(repl_keys);
    println!("MODES_RESTORED");
    std::io::stdout().flush().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !std::path::Path::new("release").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("parent must inspect and release the owned PTY");
}

struct Worker {
    child: Child,
    master: File,
    output: Vec<u8>,
}
impl Worker {
    fn until(&mut self, marker: &[u8], timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while !self.output.windows(marker.len()).any(|w| w == marker) && Instant::now() < deadline {
            let mut fd = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            unsafe {
                libc::poll(&mut fd, 1, 20);
            }
            let mut buf = [0; 8192];
            match self.master.read(&mut buf) {
                Ok(n) => self.output.extend_from_slice(&buf[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                // A PTY returns EIO after its slave closes; inspect the worker
                // output and status instead of treating that as test success.
                Err(_) => break,
            }
        }
        assert!(
            self.output.windows(marker.len()).any(|w| w == marker),
            "{}",
            String::from_utf8_lossy(&self.output)
        );
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn worker_command(root: &tempfile::TempDir, pipe: bool, mode: &str) -> Command {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "owned_pty_worker", "--nocapture"])
        .env_clear()
        .envs(common::process::contract_env(root))
        .current_dir(root.path())
        .env("DOGESH_TEST_AI_WAIT", mode)
        .env("DOGESH_TEST_AI_PIPE", pipe.to_string());
    cmd
}

fn run_on_owned_pty(mode: &str) {
    let _serial = common::serial_guard();
    for pipe in [false, true] {
        for key in [b"\x1b".as_slice(), b"\x03".as_slice()] {
            let root = tempfile::tempdir().unwrap();
            let pair = openpty(None, None).unwrap();
            let master = File::from(pair.master);
            let slave = File::from(pair.slave);
            let modes = nix::sys::termios::tcgetattr(&slave).unwrap();
            let mut cmd = worker_command(&root, pipe, mode);
            cmd.stdin(slave.try_clone().unwrap())
                .stdout(slave.try_clone().unwrap())
                .stderr(slave.try_clone().unwrap());
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
                let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
                assert!(flags >= 0);
                assert!(
                    libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0
                );
            }
            let mut worker = Worker {
                child: cmd.spawn().unwrap(),
                master,
                output: vec![],
            };
            worker.until(b"READER_READY", Duration::from_secs(5));
            worker.master.write_all(b"\t").unwrap();
            worker.until(b"REQUEST:1", Duration::from_secs(5));
            worker.master.write_all(key).unwrap();
            if mode == "direct" {
                worker.until(b"DIRECT_PENDING", Duration::from_secs(2));
            } else {
                worker.until(b"CANCELLED", Duration::from_secs(2));
                worker.master.write_all(b"r").unwrap();
                worker.until(b"READER_RESUMED", Duration::from_secs(2));
                worker.until(b"LATE_RETIRED", Duration::from_secs(5));
                worker.until(b"RETRY_DONE", Duration::from_secs(5));
            }
            worker.until(b"MODES_RESTORED", Duration::from_secs(5));
            // Darwin sets PENDIN when canonical input is restored, even when
            // crossterm restores every configured flag. Flush this owned PTY's
            // residual input before comparing modes; do not mask mode flags.
            // See xnu/bsd/kern/tty.c: ttioctl_locked and ttyflush(FREAD).
            nix::sys::termios::tcflush(&slave, nix::sys::termios::FlushArg::TCIFLUSH).unwrap();
            assert_eq!(nix::sys::termios::tcgetattr(&slave).unwrap(), modes);
            std::fs::write(root.path().join("release"), []).unwrap();
            worker.until(b"test result: ok", Duration::from_secs(5));
            assert!(worker.child.wait().unwrap().success());
        }
    }
}

#[test]
fn non_tty_wait_has_no_escape_output_or_event_reader_panic() {
    let _serial = common::serial_guard();
    for pipe in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let child = worker_command(&root, pipe, "plain")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = nix::unistd::Pid::from_raw(child.id() as i32);
        let output =
            common::process::wait_child_with_output_deadline(child, pid, Duration::from_secs(5))
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stdout.contains(&27) && !output.stderr.contains(&27));
        assert!(String::from_utf8_lossy(&output.stdout).contains("PLAIN_DONE"));
    }
}

#[test]
fn delayed_tab_requests_cancel_on_esc_and_ctrl_c_then_retry() {
    run_on_owned_pty("cancel");
}

#[test]
fn former_direct_await_cannot_consume_cancel_keys() {
    run_on_owned_pty("direct");
}
