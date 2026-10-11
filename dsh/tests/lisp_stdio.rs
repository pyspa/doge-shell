//! Lisp shell forms follow invocation mode, independently of stdin capability.
mod common;
use nix::pty::openpty;
use nix::sys::termios::{SetArg, tcgetattr, tcsetattr};
use nix::unistd::Pid;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

// Retain ownership until the bounded wait takes the child. Assertions and
// fixture I/O errors must not leave the controller or its children behind.
struct FixtureChild(Option<Child>);
impl Drop for FixtureChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = nix::sys::signal::killpg(
                Pid::from_raw(child.id() as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = child.wait();
        }
    }
}

fn run(form: &str, command: &str, file_mode: bool, input: &str) -> Output {
    let _guard = common::serial_guard();
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("payload"), "payload\n").unwrap();
    let program = format!("(print ({form} \"{command}\"))\n");
    fs::write(root.path().join("program.lisp"), &program).unwrap();
    let tty_input = matches!(input, "tty" | "full_tty");
    let mut cmd = if tty_input {
        // Darwin revokes the slave when its controlling session leader exits.
        // Keep an owned controller alive after dogesh exits, inspect modes and
        // output, then release it. No developer terminal is involved.
        let mut controller = Command::new("/bin/sh");
        controller.args([
            "-c",
            "\"$@\"; status=$?; : > completed; while [ ! -e release ]; do sleep 0.01; done; exit \"$status\"",
            "lisp-stdio-controller",
            env!("CARGO_BIN_EXE_dogesh"),
        ]);
        controller
    } else {
        Command::new(env!("CARGO_BIN_EXE_dogesh"))
    };
    cmd.current_dir(root.path())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(common::process::contract_env(&root))
        .env("SAFETY_LEVEL", "loose")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if file_mode {
        cmd.args(["lisp", "program.lisp"]);
    } else {
        cmd.args(["-l", &program]);
    }
    let mut pty = None;
    match input {
        "null" => {
            cmd.stdin(Stdio::null()).process_group(0);
        }
        "pipe" => {
            cmd.stdin(Stdio::piped()).process_group(0);
        }
        "file" => {
            cmd.stdin(File::open(root.path().join("payload")).unwrap())
                .process_group(0);
        }
        "tty" | "full_tty" => {
            let pair = openpty(None, None).unwrap();
            let slave = File::from(pair.slave);
            let mut modes = tcgetattr(&slave).unwrap();
            modes
                .local_flags
                .remove(nix::sys::termios::LocalFlags::ECHO);
            tcsetattr(&slave, SetArg::TCSANOW, &modes).unwrap();
            cmd.stdin(slave.try_clone().unwrap());
            if input == "full_tty" {
                cmd.stdout(slave.try_clone().unwrap())
                    .stderr(slave.try_clone().unwrap());
            }
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            pty = Some((File::from(pair.master), slave, modes));
        }
        _ => unreachable!(),
    }
    let mut owned = FixtureChild(Some(cmd.spawn().unwrap()));
    let child = owned.0.as_mut().unwrap();
    if input == "pipe" {
        child.stdin.take().unwrap().write_all(b"payload\n").unwrap();
    }
    let pid = Pid::from_raw(child.id() as i32);
    let mut pty_output = Vec::new();
    let mut observed_modes = None;
    if let Some((master, slave, _modes)) = &mut pty {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.path().join("completed").exists() && Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if !root.path().join("completed").exists() {
            panic!("owned PTY controller did not reach the inspection barrier");
        }
        observed_modes = Some(tcgetattr(slave));
        if input == "full_tty" {
            unsafe {
                let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
                assert!(flags >= 0);
                assert!(
                    libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0
                );
            }
            let mut bytes = [0; 4096];
            loop {
                match master.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) => pty_output.extend_from_slice(&bytes[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        panic!("owned PTY read: {e}");
                    }
                }
            }
        }
        fs::write(root.path().join("release"), []).unwrap();
    }
    let mut result = common::process::wait_child_with_output_deadline(
        owned.0.take().unwrap(),
        pid,
        Duration::from_secs(10),
    )
    .unwrap();
    result.stdout.extend(pty_output);
    if let Some((_master, _slave, modes)) = pty {
        assert_eq!(
            observed_modes.unwrap().unwrap(),
            modes,
            "script must preserve owned terminal modes"
        );
    }
    result
}

#[test]
fn shell_forms_accept_null_pipe_and_file_stdin_in_both_program_modes() {
    for input in ["null", "pipe", "file"] {
        for file_mode in [false, true] {
            for form in ["sh", "sh!"] {
                for command in [
                    "echo payload",
                    "cat payload",
                    "cat < payload",
                    "cat payload | cat",
                    "cat payload > saved; cat saved",
                ] {
                    let result = run(form, command, file_mode, input);
                    assert!(
                        result.status.success(),
                        "{input} {form} {command}: {}",
                        String::from_utf8_lossy(&result.stderr)
                    );
                    assert!(!result.stdout.contains(&27) && !result.stderr.contains(&27));
                    assert!(String::from_utf8_lossy(&result.stdout).contains("payload"));
                }
            }
        }
    }
}

#[test]
fn tty_backed_scripts_are_not_interactive_and_leave_terminal_modes_unchanged() {
    for input in ["tty", "full_tty"] {
        for file_mode in [false, true] {
            for form in ["sh", "sh!"] {
                let result = run(form, "cat payload | cat", file_mode, input);
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
                assert!(
                    !result.stdout.contains(&27),
                    "script emitted terminal controls: {:?}",
                    result.stdout
                );
            }
        }
    }
}

#[test]
fn shell_forms_consume_inherited_stdin_and_null_reaches_eof() {
    for input in ["null", "pipe", "file"] {
        for file_mode in [false, true] {
            for form in ["sh", "sh!"] {
                let result = run(form, "cat", file_mode, input);
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
                assert_eq!(
                    String::from_utf8_lossy(&result.stdout).contains("payload"),
                    input != "null"
                );
                assert!(!result.stdout.contains(&27) && !result.stderr.contains(&27));
            }
        }
    }
}
