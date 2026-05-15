use anyhow::Result;
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const READ_POLL_INTERVAL: Duration = Duration::from_millis(5);
const EXIT_DRAIN_GRACE: Duration = Duration::from_millis(200);
const MAX_STDOUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainStatus {
    Eof,
    WouldBlock,
    DeadlineReached,
    OutputLimitReached,
}

pub(crate) fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    command
}

pub(crate) fn shell_command(command_template: &str) -> Command {
    let mut cmd = command("sh");
    cmd.arg("-c").arg(command_template);
    cmd
}

pub(crate) fn collect_stdout(mut command: Command, timeout: Duration) -> Result<String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    wait_and_collect_stdout(&mut child, timeout)
}

fn wait_and_collect_stdout(child: &mut Child, timeout: Duration) -> Result<String> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Child stdout not captured"))?;
    set_nonblocking(&stdout)?;

    let started = Instant::now();
    let deadline = deadline_after(started, timeout);
    let mut output = Vec::new();

    loop {
        match drain_available_stdout(&mut stdout, &mut output, deadline)? {
            DrainStatus::DeadlineReached | DrainStatus::OutputLimitReached => {
                terminate_child(child);
                let _ = child.wait();
                return Ok(String::new());
            }
            DrainStatus::Eof | DrainStatus::WouldBlock => {}
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                let drain_status = drain_stdout_after_exit(&mut stdout, &mut output)?;
                if drain_status == DrainStatus::OutputLimitReached {
                    terminate_child(child);
                    return Ok(String::new());
                }
                if drain_status == DrainStatus::DeadlineReached {
                    terminate_child(child);
                }
                return if status.success() {
                    Ok(String::from_utf8(output)?)
                } else {
                    Ok(String::new())
                };
            }
            Ok(None) => {}
            Err(err) if child_was_reaped(&err) => {
                let drain_status = drain_stdout_after_exit(&mut stdout, &mut output)?;
                if drain_status == DrainStatus::OutputLimitReached {
                    terminate_child(child);
                    return Ok(String::new());
                }
                if drain_status == DrainStatus::DeadlineReached {
                    terminate_child(child);
                }
                return Ok(String::from_utf8(output)?);
            }
            Err(err) => return Err(err.into()),
        }

        if started.elapsed() >= timeout {
            terminate_child(child);
            let _ = child.wait();
            return Ok(String::new());
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        std::thread::sleep(remaining.min(READ_POLL_INTERVAL));
    }
}

fn deadline_after(started: Instant, timeout: Duration) -> Instant {
    started.checked_add(timeout).unwrap_or(started)
}

#[cfg(unix)]
fn child_was_reaped(err: &io::Error) -> bool {
    err.raw_os_error() == Some(libc::ECHILD)
}

#[cfg(not(unix))]
fn child_was_reaped(_err: &io::Error) -> bool {
    false
}

fn drain_stdout_after_exit(
    stdout: &mut std::process::ChildStdout,
    output: &mut Vec<u8>,
) -> io::Result<DrainStatus> {
    let deadline = Instant::now() + EXIT_DRAIN_GRACE;
    loop {
        let before = output.len();
        match drain_available_stdout(stdout, output, deadline)? {
            DrainStatus::Eof => return Ok(DrainStatus::Eof),
            DrainStatus::DeadlineReached => return Ok(DrainStatus::DeadlineReached),
            DrainStatus::OutputLimitReached => return Ok(DrainStatus::OutputLimitReached),
            DrainStatus::WouldBlock => {}
        }
        if Instant::now() >= deadline {
            return Ok(DrainStatus::DeadlineReached);
        }
        if output.len() == before {
            std::thread::sleep(READ_POLL_INTERVAL);
        }
    }
}

fn drain_available_stdout(
    stdout: &mut std::process::ChildStdout,
    output: &mut Vec<u8>,
    deadline: Instant,
) -> io::Result<DrainStatus> {
    let mut buf = [0_u8; 8192];
    loop {
        if Instant::now() >= deadline {
            return Ok(DrainStatus::DeadlineReached);
        }
        if output.len() >= MAX_STDOUT_BYTES {
            return Ok(DrainStatus::OutputLimitReached);
        }

        let read_len = buf.len().min(MAX_STDOUT_BYTES - output.len());
        match stdout.read(&mut buf[..read_len]) {
            Ok(0) => return Ok(DrainStatus::Eof),
            Ok(n) => output.extend_from_slice(&buf[..n]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                return Ok(DrainStatus::WouldBlock);
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(unix)]
fn set_nonblocking(stdout: &std::process::ChildStdout) -> io::Result<()> {
    let fd = stdout.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_nonblocking(_stdout: &std::process::ChildStdout) -> io::Result<()> {
    Ok(())
}

fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(child.id() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[cfg(unix)]
    fn write_executable_script(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_descendants_in_process_group() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("holds-stdout.sh");
        let survived = dir.path().join("survived.txt");
        write_executable_script(
            &script,
            "#!/bin/sh\n(sleep 2; printf survived > survived.txt) &\nwait\n",
        );

        let mut command = command(script.to_str().unwrap());
        command.current_dir(dir.path());
        let started = Instant::now();
        let output = collect_stdout(command, Duration::from_millis(300)).unwrap();

        assert_eq!(output, "");
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1800));
        assert!(
            !survived.exists(),
            "timeout should kill descendant processes in the subprocess group"
        );
    }

    #[cfg(unix)]
    #[test]
    fn successful_child_does_not_wait_for_stdout_holding_background_process() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("background-stdout.sh");
        write_executable_script(&script, "#!/bin/sh\n(sleep 4; printf late) &\nexit 0\n");

        let mut command = command(script.to_str().unwrap());
        command.current_dir(dir.path());
        let started = Instant::now();
        let output = collect_stdout(command, Duration::from_millis(1500)).unwrap();

        assert_eq!(output, "");
        assert!(started.elapsed() < Duration::from_millis(1500));
    }

    #[cfg(unix)]
    #[test]
    fn continuous_stdout_is_bounded_by_timeout() {
        let started = Instant::now();
        let output = collect_stdout(
            shell_command("while :; do printf 0123456789abcdef; done"),
            Duration::from_millis(100),
        )
        .unwrap();

        assert_eq!(output, "");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
