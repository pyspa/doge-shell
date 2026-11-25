//! Exercise the diagnosis UI in its own session, with a private input terminal.
mod common;

use crossterm::terminal::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled};
use doge_shell::ai_features::ui::{AiChatUi, DiagnosticContext, UiOutcome};
use nix::pty::{Winsize, openpty};
use nix::sys::termios::{FlushArg, tcflush, tcgetattr};
use nix::unistd::{Pid, dup, dup2_stdin, dup2_stdout, setsid};
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

fn check_on_owned_pty() {
    let size = Winsize {
        ws_row: 8,
        ws_col: 32,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = openpty(Some(&size), None).unwrap();
    let mut master = File::from(pty.master);
    let slave = pty.slave;
    dup2_stdin(&slave).unwrap();
    let output = dup(io::stdout()).unwrap();
    let mut ui = AiChatUi::new(
        DiagnosticContext {
            command: "false".into(),
            output: String::new(),
            exit_code: 1,
        },
        "diagnosis".into(),
    );

    // A broken output pipe fails the initial EnterAlternateScreen write on
    // both Linux and macOS, after raw mode has been enabled on the slave.
    let cooked = tcgetattr(&slave).unwrap();
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    dup2_stdout(writer).unwrap();
    let failed = ui.run();
    dup2_stdout(&output).unwrap();
    let Err(error) = failed else {
        panic!("alternate-screen setup must fail");
    };
    assert_eq!(
        error.downcast_ref::<io::Error>().map(io::Error::kind),
        Some(io::ErrorKind::BrokenPipe)
    );
    assert!(!is_raw_mode_enabled().unwrap());
    tcflush(&slave, FlushArg::TCIFLUSH).unwrap(); // clear Darwin's PENDIN
    assert_eq!(tcgetattr(&slave).unwrap(), cooked);

    // The REPL already owns raw mode when it opens the diagnosis screen.
    enable_raw_mode().unwrap();
    let raw = tcgetattr(&slave).unwrap();
    master.write_all(b"\x1b").unwrap();
    dup2_stdout(&slave).unwrap();
    let quit = ui.run();
    dup2_stdout(output).unwrap();
    assert!(matches!(quit.unwrap(), UiOutcome::Quit));
    assert!(is_raw_mode_enabled().unwrap());
    assert_eq!(tcgetattr(&slave).unwrap(), raw);
    disable_raw_mode().unwrap();
}

#[test]
fn diagnosis_preserves_entry_raw_mode() {
    const WORKER: &str = "DOGESH_TEST_DIAGNOSIS_TERMINAL_WORKER";
    if std::env::var_os(WORKER).is_some() {
        check_on_owned_pty();
        return;
    }

    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "diagnosis_preserves_entry_raw_mode",
            "--nocapture",
        ])
        .env(WORKER, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: setsid is async-signal-safe. Detaching also prevents any
    // crossterm /dev/tty fallback from reaching the developer's terminal.
    unsafe {
        command.pre_exec(|| setsid().map(|_| ()).map_err(io::Error::from));
    }
    let child = command.spawn().unwrap();
    let pid = Pid::from_raw(child.id() as i32);
    let output = common::process::wait_child_with_output_deadline(
        child,
        pid,
        common::process::DEFAULT_CASE_TIMEOUT,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn noninteractive_chat_preserves_terminal_signal_mode() {
    use nix::sys::termios::{LocalFlags, SetArg, tcsetattr};
    let pty = openpty(None, None).unwrap();
    let mut flags = tcgetattr(&pty.slave).unwrap();
    flags.local_flags.remove(LocalFlags::ISIG);
    tcsetattr(&pty.slave, SetArg::TCSANOW, &flags).unwrap();

    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_dogesh"));
    command
        .args(["-c", "!"])
        .env_clear()
        .envs(common::process::contract_env(&home))
        .current_dir(home.path())
        .stdin(File::from(dup(&pty.slave).unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: detach from the test runner's controlling terminal before exec.
    unsafe {
        command.pre_exec(|| setsid().map(|_| ()).map_err(io::Error::from));
    }
    let child = command.spawn().unwrap();
    let pid = Pid::from_raw(child.id() as i32);
    let output = common::process::wait_child_with_output_deadline(
        child,
        pid,
        common::process::DEFAULT_CASE_TIMEOUT,
    )
    .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("message"),
        "{output:?}"
    );
    assert!(
        !tcgetattr(&pty.slave)
            .unwrap()
            .local_flags
            .contains(LocalFlags::ISIG)
    );
}
