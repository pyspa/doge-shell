//! Byte capture on an owned controlling PTY; never print terminal payloads.
mod common;

use nix::pty::{Winsize, openpty};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Capture {
    child: Child,
    master: File,
    bytes: Vec<u8>,
    file: File,
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Capture {
    fn pump(&mut self, predicate: impl Fn(&[u8]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let mut buffer = [0; 8192];
            match self.master.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    self.file.write_all(&buffer[..n]).unwrap();
                    self.bytes.extend_from_slice(&buffer[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
            if predicate(&self.bytes) {
                return;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            predicate(&self.bytes),
            "owned PTY deadline; raw bytes retained only in capture file"
        );
    }
}

fn count(bytes: &[u8], needle: &[u8]) -> usize {
    bytes.windows(needle.len()).filter(|w| *w == needle).count()
}

#[test]
fn hostile_cwd_is_data_on_owned_pty_and_redraw_keeps_markers() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root
        .path()
        .join("repo\x07\x1b]52;c;payload\x1b\\\r\n\u{85}\u{9c}日本 %?#");
    fs::create_dir(&cwd).unwrap();
    fs::create_dir(cwd.join(".git")).unwrap();
    let pty = openpty(
        Some(&Winsize {
            ws_row: 40,
            ws_col: 160,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }),
        None,
    )
    .unwrap();
    let master = File::from(pty.master);
    let slave = File::from(pty.slave);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dogesh"));
    cmd.current_dir(&cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(common::process::contract_env(&root))
        .env("TERM", "xterm")
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
    let mut capture = Capture {
        child: cmd.spawn().unwrap(),
        master,
        bytes: vec![],
        file: File::create(root.path().join("terminal.bin")).unwrap(),
    };
    let a = b"\x1b]133;A\x1b\\";
    let b = b"\x1b]133;B\x1b\\";
    capture.pump(|bytes| count(bytes, b) == 1);
    assert_eq!(count(&capture.bytes, a), 1);
    assert_eq!(count(&capture.bytes, b"\x1b]7;file://"), 1);
    assert_eq!(count(&capture.bytes, b"\x1b]52;"), 0);
    assert_eq!(
        count(
            &capture.bytes,
            b"%07%1B%5D52%3Bc%3Bpayload%1B%5C%0D%0A%C2%85%C2%9C%E6%97%A5%E6%9C%AC%20%25%3F%23"
        ),
        1
    );
    assert!(
        count(
            &capture.bytes,
            br"repo\u{7}\u{1b}]52;c;payload\u{1b}\\r\n\u{85}\u{9c}"
        ) >= 1
    );
    let before = capture.bytes.len();
    // Editing the current input redraws it without starting a new prompt.
    capture.master.write_all(b"x").unwrap();
    let redraw_started = Instant::now();
    capture.pump(|bytes| {
        redraw_started.elapsed() >= Duration::from_millis(150)
            && bytes.len() > before
            && count(&bytes[before..], b"x") > 0
    });
    assert_eq!(count(&capture.bytes, a), 1);
    assert_eq!(count(&capture.bytes, b), 1);
    assert_eq!(count(&capture.bytes, b"\x1b]52;"), 0);
    assert!(cwd.is_dir());
}
