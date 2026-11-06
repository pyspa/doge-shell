//! Enter behavior on a bounded scratch PTY, with isolated configuration/history.
use super::{common, support};

use nix::pty::{Winsize, openpty};
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PROMPT: &[u8] = b"\x1b]133;B\x1b\\";

struct Session {
    child: support::OwnedChild,
    master: File,
    output: Vec<u8>,
    deadline: Instant,
}

impl Session {
    fn new(root: &tempfile::TempDir) -> Self {
        let pair = openpty(
            Some(&Winsize {
                ws_row: 40,
                ws_col: 120,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )
        .unwrap();
        let master = File::from(pair.master);
        let slave = File::from(pair.slave);
        support::nonblocking(&master).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_dogesh"));
        command
            .env_clear()
            .envs(common::process::contract_env(root))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TERM", "xterm")
            .current_dir(root.path())
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Self {
            child: support::OwnedChild(command.spawn().unwrap()),
            master,
            output: Vec::new(),
            deadline: Instant::now() + Duration::from_secs(30),
        }
    }

    fn read(&mut self) -> io::Result<()> {
        match support::pump(&mut self.master, &mut self.output) {
            Err(error) if error.raw_os_error() == Some(libc::EIO) => Ok(()),
            result => result,
        }
    }

    fn send(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        let deadline = support::phase_deadline(self.deadline, Duration::from_secs(2));
        while !bytes.is_empty() {
            self.read()?;
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self.master.write(&bytes[..bytes.len().min(64)]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn until(&mut self, start: usize, needles: &[&[u8]]) -> bool {
        let deadline = support::phase_deadline(self.deadline, Duration::from_secs(5));
        while Instant::now() < deadline {
            self.read().unwrap();
            if needles
                .iter()
                .any(|needle| contains(&self.output[start..], needle))
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    fn finish(&mut self) {
        // Recover continuation before builtin exit, including the RED path.
        let start = self.output.len();
        self.send(b"\x03").unwrap();
        assert!(self.until(start, &[b"\x1b]133;D;130"]));
        self.send(b"exit\r").unwrap();
        let (master, output) = (&mut self.master, &mut self.output);
        let mut drain = || {
            let _ = support::pump(master, output);
        };
        if !support::wait_until(
            &mut self.child,
            Instant::now() + Duration::from_secs(2),
            &mut drain,
        )
        .unwrap()
        {
            support::kill_owned(&mut self.child, drain).unwrap();
            panic!("scratch shell did not exit gracefully");
        }
    }
}

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|part| part == needle)
}

fn contains_output(bytes: &[u8], expected: &[u8]) -> bool {
    // The shell's nested foreground PTY may apply ONLCR a second time.
    let normalized: Vec<_> = bytes
        .iter()
        .copied()
        .filter(|byte| *byte != b'\r')
        .collect();
    contains(&normalized, expected)
}

#[test]
fn enter_executes_escaped_literals_and_continues_real_pipes() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::new(&root);
    assert!(session.until(0, &[PROMPT]));
    let mut failure = None;
    for (command, expected) in [
        (b"echo \\|\r".as_slice(), b"|\n".as_slice()),
        (b"echo \\|:\r", b"|:\n"),
        (b"echo \\&\\&\r", b"&&\n"),
        (b"echo \\|\\|\r", b"||\n"),
    ] {
        let start = session.output.len();
        session.send(command).unwrap();
        let ready = session.until(start, &[PROMPT, b"..> "]);
        let output = &session.output[start..];
        if !ready || !contains_output(output, expected) || contains(output, b"..> ") {
            failure = Some(format!(
                "escaped literal did not execute: {command:?}; ready={ready}; captured={:?}",
                String::from_utf8_lossy(output)
            ));
            break;
        }
    }
    if failure.is_none() {
        for (command, expected) in [
            (b"echo PIPE |\r".as_slice(), b"PIPE\n".as_slice()),
            (b"echo \\||\r", b"|\n"),
        ] {
            let start = session.output.len();
            session.send(command).unwrap();
            assert!(session.until(start, &[b"..> "]));
            assert!(!contains(&session.output[start..], PROMPT));
            let start = session.output.len();
            session.send(b"cat\r").unwrap();
            assert!(session.until(start, &[PROMPT]));
            assert!(
                contains_output(&session.output[start..], expected),
                "pipeline output: {:?}",
                String::from_utf8_lossy(&session.output[start..])
            );
        }
    }
    session.finish();
    assert!(failure.is_none(), "{}", failure.unwrap_or_default());
}
