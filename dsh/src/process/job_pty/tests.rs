use super::{
    AsyncStdin, PtyMasterUse, PtySetupOps, capture_output_and_history, cleanup_pty_tasks,
    is_builtin_job, setup_pty, setup_pty_with, setup_pty_with_ops, should_create_pty,
    should_enable_foreground_pty_raw_mode, uses_full_pty_proxy,
};
use crate::environment::Environment;
use crate::process::async_io::AsyncPtyMasterWriter;
use crate::process::io::PtyMonitor;
use crate::process::pty::PtyMode;
use crate::process::{BuiltinProcess, Job, JobProcess, Process, ProcessState, Pty};
use crate::shell::Shell;
use dsh_types::Context;
use dsh_types::ExitStatus;
use dsh_types::observed_output::SharedOutputObserver;
use dsh_types::terminal::{ShellMode, TerminalState};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::unistd::Pid;
use std::fs::File;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::io::IntoRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::oneshot;

/// Deterministic fault injector for the PTY setup seam.
///
/// Each flag fails exactly one infrastructure step; counters record
/// which steps ran so tests can assert the OutputOnly path never
/// touches input resources. No fd exhaustion, no timing, no real
/// terminal.
struct FaultInjectingOps {
    fail_pty_new: bool,
    fail_output_clone: bool,
    fail_monitor: bool,
    fail_input_clone: bool,
    fail_writer: bool,
    output_clone_calls: AtomicUsize,
    input_clone_calls: AtomicUsize,
    monitor_calls: AtomicUsize,
    writer_calls: AtomicUsize,
}

impl FaultInjectingOps {
    fn clean() -> Self {
        Self {
            fail_pty_new: false,
            fail_output_clone: false,
            fail_monitor: false,
            fail_input_clone: false,
            fail_writer: false,
            output_clone_calls: AtomicUsize::new(0),
            input_clone_calls: AtomicUsize::new(0),
            monitor_calls: AtomicUsize::new(0),
            writer_calls: AtomicUsize::new(0),
        }
    }
}

impl PtySetupOps for FaultInjectingOps {
    fn new_pty(&self) -> anyhow::Result<Pty> {
        if self.fail_pty_new {
            return Err(anyhow::anyhow!("injected Pty::new failure"));
        }
        Pty::new()
    }

    fn clone_master(&self, pty: &Pty, purpose: PtyMasterUse) -> anyhow::Result<File> {
        match purpose {
            PtyMasterUse::Output => {
                self.output_clone_calls.fetch_add(1, Ordering::SeqCst);
                if self.fail_output_clone {
                    return Err(anyhow::anyhow!("injected output clone failure"));
                }
            }
            PtyMasterUse::Input => {
                self.input_clone_calls.fetch_add(1, Ordering::SeqCst);
                if self.fail_input_clone {
                    return Err(anyhow::anyhow!("injected input clone failure"));
                }
            }
        }
        pty.try_clone_master()
    }

    fn new_monitor(
        &self,
        master: File,
        observer: Option<SharedOutputObserver>,
    ) -> anyhow::Result<PtyMonitor> {
        self.monitor_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_monitor {
            drop(master);
            return Err(anyhow::anyhow!("injected PtyMonitor failure"));
        }
        PtyMonitor::new(master.into_raw_fd(), observer)
    }

    fn new_writer(&self, master: File) -> std::io::Result<AsyncPtyMasterWriter> {
        self.writer_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_writer {
            drop(master);
            return Err(std::io::Error::other("injected writer failure"));
        }
        AsyncPtyMasterWriter::new(master)
    }
}

/// Opener that records whether the input proxy ever asked for input.
///
/// OutputOnly and downgraded setups must drop the opener without calling
/// it, so a call here means the setup touched terminal input it should
/// not have.
fn recording_opener(
    called: Arc<AtomicBool>,
) -> impl FnOnce() -> std::io::Result<AsyncStdin> + Send + 'static {
    move || {
        called.store(true, Ordering::SeqCst);
        Err(std::io::Error::other("input must not be opened"))
    }
}

fn full_proxy_job() -> Job {
    test_job_with_process(
        JobProcess::Command(Process::new(
            "/bin/echo".to_string(),
            vec!["echo".to_string()],
        )),
        false,
    )
}

struct DropNotify(Option<oneshot::Sender<()>>);

impl Drop for DropNotify {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

fn test_context(foreground: bool, interactive: bool) -> Context {
    Context {
        shell_pid: Pid::from_raw(1),
        shell_pgid: Pid::from_raw(1),
        shell_tmode: None,
        terminal_state: TerminalState::non_terminal(),
        shell_mode: if interactive {
            ShellMode::Interactive
        } else {
            ShellMode::Script
        },
        foreground,
        interactive,
        infile: STDIN_FILENO,
        outfile: STDOUT_FILENO,
        errfile: STDERR_FILENO,
        captured_out: None,
        output_observer: None,
        save_history: true,
        pid: None,
        pgid: None,
        process_count: 0,
    }
}

fn test_builtin(
    _ctx: &Context,
    _argv: Vec<String>,
    _proxy: &mut dyn dsh_builtin::ShellProxy,
) -> ExitStatus {
    ExitStatus::ExitedWith(0)
}

fn test_job_with_process(process: JobProcess, with_pty: bool) -> Job {
    let mut job = Job::new("test".to_string(), Pid::from_raw(1));
    job.set_process(process);
    if with_pty {
        job.pty = Some(Pty::new().expect("failed to create test pty"));
        job.pty_mode = Some(PtyMode::FullProxy);
    }
    job
}

#[test]
fn should_create_pty_only_for_foreground_interactive_jobs() {
    let interactive_foreground = test_context(true, true);
    let interactive_background = test_context(false, true);
    let non_interactive_foreground = test_context(true, false);

    assert!(should_create_pty(&interactive_foreground, false, false));
    assert!(!should_create_pty(&interactive_background, false, false));
    assert!(!should_create_pty(
        &non_interactive_foreground,
        false,
        false
    ));
}

#[test]
fn should_create_pty_respects_disable_flags() {
    let ctx = test_context(true, true);

    assert!(!should_create_pty(&ctx, true, false));
    assert!(!should_create_pty(&ctx, false, true));
}

#[test]
fn detects_builtin_jobs() {
    let builtin_job = test_job_with_process(
        JobProcess::Builtin(BuiltinProcess::new(
            "aic".to_string(),
            test_builtin,
            vec!["aic".to_string()],
        )),
        false,
    );
    let command_job = test_job_with_process(
        JobProcess::Command(Process::new(
            "/bin/echo".to_string(),
            vec!["echo".to_string()],
        )),
        false,
    );

    assert!(is_builtin_job(&builtin_job));
    assert!(!is_builtin_job(&command_job));
}

#[test]
fn foreground_raw_mode_is_limited_to_full_proxy_pty_jobs() {
    let ctx = test_context(true, true);
    let builtin_job = test_job_with_process(
        JobProcess::Builtin(BuiltinProcess::new(
            "aic".to_string(),
            test_builtin,
            vec!["aic".to_string()],
        )),
        true,
    );
    let command_job = test_job_with_process(
        JobProcess::Command(Process::new(
            "/bin/echo".to_string(),
            vec!["echo".to_string()],
        )),
        true,
    );
    let mut output_only_job = test_job_with_process(
        JobProcess::Command(Process::new(
            "/bin/echo".to_string(),
            vec!["echo".to_string()],
        )),
        true,
    );
    output_only_job.pty_mode = Some(PtyMode::OutputOnly);

    assert!(!should_enable_foreground_pty_raw_mode(&builtin_job, &ctx));
    assert!(should_enable_foreground_pty_raw_mode(&command_job, &ctx));
    assert!(!should_enable_foreground_pty_raw_mode(
        &output_only_job,
        &ctx
    ));
    assert!(uses_full_pty_proxy(&command_job));
    assert!(!uses_full_pty_proxy(&output_only_job));
}

#[tokio::test]
async fn setup_pty_uses_full_proxy_with_input_proxy_for_terminal_output() {
    let mut ctx = test_context(true, true);
    let mut job = test_job_with_process(
        JobProcess::Command(Process::new(
            "/bin/echo".to_string(),
            vec!["echo".to_string()],
        )),
        false,
    );

    // The production opener reads the real controlling terminal, which
    // would eat the keystrokes of whoever is running `cargo test`. Give
    // the proxy a PTY of its own instead. `scratch` outlives the proxy
    // task so the reopen below cannot fail into the stdin fallback.
    let scratch = Pty::new().expect("scratch pty for the input proxy");
    let scratch_fd = scratch.slave.as_raw_fd();
    let child = setup_pty_with(&mut job, &mut ctx, move || {
        AsyncStdin::open_tty_from_fd(unsafe { BorrowedFd::borrow_raw(scratch_fd) })
    })
    .await
    .expect("setup pty");

    let config = child.expect("FullProxy setup must return a child config");
    assert_eq!(config.mode, PtyMode::FullProxy);
    assert_eq!(job.pty_mode, Some(PtyMode::FullProxy));
    // The returned child mode and the committed job mode must agree:
    // a stale FullProxy here would wire child stdin to the PTY slave
    // with no input proxy and skip the parent setpgid path.
    assert_eq!(job.pty_mode, Some(config.mode));
    assert!(job.pty.is_some());
    assert!(job.pty_output_task.is_some());
    assert!(job.pty_input_task.is_some());
    assert!(uses_full_pty_proxy(&job));
    assert!(should_enable_foreground_pty_raw_mode(&job, &ctx));

    cleanup_pty_tasks(&mut job).await;
    drop(scratch);
}

#[tokio::test]
async fn setup_pty_falls_back_to_output_only_when_redirected() {
    let mut ctx = test_context(true, true);
    // Output is redirected away from the terminal, so no full proxy.
    ctx.outfile = 5;
    let mut job = test_job_with_process(
        JobProcess::Command(Process::new(
            "/bin/echo".to_string(),
            vec!["echo".to_string()],
        )),
        false,
    );

    // Safe with the production opener: OutputOnly never calls it, so the
    // real terminal is untouched even under `cargo test` from a tty.
    let child = setup_pty(&mut job, &mut ctx).await.expect("setup pty");

    let config = child.expect("OutputOnly setup must return a child config");
    assert_eq!(config.mode, PtyMode::OutputOnly);
    assert_eq!(job.pty_mode, Some(PtyMode::OutputOnly));
    assert_eq!(job.pty_mode, Some(config.mode));
    assert!(job.pty.is_some());
    assert!(job.pty_output_task.is_some());
    assert!(job.pty_input_task.is_none());
    assert!(!uses_full_pty_proxy(&job));
    assert!(!should_enable_foreground_pty_raw_mode(&job, &ctx));

    cleanup_pty_tasks(&mut job).await;
}

#[tokio::test]
async fn setup_pty_input_clone_failure_degrades_to_output_only() {
    let mut ctx = test_context(true, true);
    let mut job = full_proxy_job();
    let ops = FaultInjectingOps {
        fail_input_clone: true,
        ..FaultInjectingOps::clean()
    };
    let opened = Arc::new(AtomicBool::new(false));
    let opened_clone = Arc::clone(&opened);

    let child = setup_pty_with_ops(&mut job, &mut ctx, recording_opener(opened_clone), &ops)
        .await
        .expect("setup pty");

    let config = child.expect("input clone failure must downgrade, not discard PTY");
    assert_eq!(config.mode, PtyMode::OutputOnly);
    assert_eq!(job.pty_mode, Some(PtyMode::OutputOnly));
    assert_eq!(job.pty_mode, Some(config.mode));
    assert!(job.pty.is_some());
    assert!(job.pty_output_task.is_some());
    assert!(job.pty_input_task.is_none());
    assert!(!uses_full_pty_proxy(&job));
    assert!(!should_enable_foreground_pty_raw_mode(&job, &ctx));
    assert!(
        !opened.load(Ordering::SeqCst),
        "downgraded setup must not open terminal input"
    );

    cleanup_pty_tasks(&mut job).await;
}

#[tokio::test]
async fn setup_pty_input_writer_failure_degrades_to_output_only() {
    let mut ctx = test_context(true, true);
    let mut job = full_proxy_job();
    let ops = FaultInjectingOps {
        fail_writer: true,
        ..FaultInjectingOps::clean()
    };
    let opened = Arc::new(AtomicBool::new(false));
    let opened_clone = Arc::clone(&opened);

    let child = setup_pty_with_ops(&mut job, &mut ctx, recording_opener(opened_clone), &ops)
        .await
        .expect("setup pty");

    let config = child.expect("writer failure must downgrade, not discard PTY");
    assert_eq!(config.mode, PtyMode::OutputOnly);
    assert_eq!(job.pty_mode, Some(PtyMode::OutputOnly));
    assert_eq!(job.pty_mode, Some(config.mode));
    assert!(job.pty.is_some());
    assert!(job.pty_output_task.is_some());
    assert!(job.pty_input_task.is_none());
    assert!(!uses_full_pty_proxy(&job));
    assert!(!should_enable_foreground_pty_raw_mode(&job, &ctx));
    assert!(
        !opened.load(Ordering::SeqCst),
        "downgraded setup must not open terminal input"
    );

    cleanup_pty_tasks(&mut job).await;
}

#[tokio::test]
async fn setup_pty_output_clone_failure_falls_back_to_normal_execution() {
    let mut ctx = test_context(true, true);
    let mut job = full_proxy_job();
    let ops = FaultInjectingOps {
        fail_output_clone: true,
        ..FaultInjectingOps::clean()
    };
    let opened = Arc::new(AtomicBool::new(false));
    let opened_clone = Arc::clone(&opened);

    let child = setup_pty_with_ops(&mut job, &mut ctx, recording_opener(opened_clone), &ops)
        .await
        .expect("setup pty");

    // No output monitor exists, so no PTY may reach the child.
    assert!(child.is_none());
    assert!(job.pty.is_none());
    assert!(job.pty_mode.is_none());
    assert!(job.pty_output_task.is_none());
    assert!(job.pty_input_task.is_none());
    assert!(
        !opened.load(Ordering::SeqCst),
        "failed setup must not open terminal input"
    );
}

#[tokio::test]
async fn setup_pty_output_monitor_failure_falls_back_to_normal_execution() {
    let mut ctx = test_context(true, true);
    let mut job = full_proxy_job();
    let ops = FaultInjectingOps {
        fail_monitor: true,
        ..FaultInjectingOps::clean()
    };
    let opened = Arc::new(AtomicBool::new(false));
    let opened_clone = Arc::clone(&opened);

    let child = setup_pty_with_ops(&mut job, &mut ctx, recording_opener(opened_clone), &ops)
        .await
        .expect("setup pty");

    assert!(child.is_none());
    assert!(job.pty.is_none());
    assert!(job.pty_mode.is_none());
    assert!(job.pty_output_task.is_none());
    assert!(job.pty_input_task.is_none());
    assert!(
        !opened.load(Ordering::SeqCst),
        "failed setup must not open terminal input"
    );
}

#[tokio::test]
async fn setup_pty_new_failure_falls_back_to_normal_execution() {
    let mut ctx = test_context(true, true);
    let mut job = full_proxy_job();
    let ops = FaultInjectingOps {
        fail_pty_new: true,
        ..FaultInjectingOps::clean()
    };
    let opened = Arc::new(AtomicBool::new(false));
    let opened_clone = Arc::clone(&opened);

    let child = setup_pty_with_ops(&mut job, &mut ctx, recording_opener(opened_clone), &ops)
        .await
        .expect("setup pty");

    assert!(child.is_none());
    assert!(job.pty.is_none());
    assert!(job.pty_mode.is_none());
    assert!(job.pty_output_task.is_none());
    assert!(job.pty_input_task.is_none());
    assert!(
        !opened.load(Ordering::SeqCst),
        "failed setup must not open terminal input"
    );
}

#[tokio::test]
async fn setup_pty_output_only_does_not_prepare_input() {
    let mut ctx = test_context(true, true);
    // Redirected output selects OutputOnly before any input work.
    ctx.outfile = 5;
    let mut job = full_proxy_job();
    let ops = FaultInjectingOps::clean();
    let opened = Arc::new(AtomicBool::new(false));
    let opened_clone = Arc::clone(&opened);

    let child = setup_pty_with_ops(&mut job, &mut ctx, recording_opener(opened_clone), &ops)
        .await
        .expect("setup pty");

    let config = child.expect("OutputOnly setup must return a child config");
    assert_eq!(config.mode, PtyMode::OutputOnly);
    assert_eq!(job.pty_mode, Some(config.mode));
    assert!(job.pty_input_task.is_none());
    assert_eq!(ops.input_clone_calls.load(Ordering::SeqCst), 0);
    assert_eq!(ops.writer_calls.load(Ordering::SeqCst), 0);
    assert!(
        !opened.load(Ordering::SeqCst),
        "OutputOnly must not open terminal input"
    );

    cleanup_pty_tasks(&mut job).await;
}

#[tokio::test]
async fn setup_pty_child_config_mode_matches_job_mode() {
    // FullProxy success: both sides agree on FullProxy.
    let mut ctx = test_context(true, true);
    let mut job = full_proxy_job();
    let ops = FaultInjectingOps::clean();
    let scratch = Pty::new().expect("scratch pty for the input proxy");
    let scratch_fd = scratch.slave.as_raw_fd();
    let child = setup_pty_with_ops(
        &mut job,
        &mut ctx,
        move || AsyncStdin::open_tty_from_fd(unsafe { BorrowedFd::borrow_raw(scratch_fd) }),
        &ops,
    )
    .await
    .expect("setup pty");
    let config = child.expect("FullProxy setup must return a child config");
    assert_eq!(config.mode, PtyMode::FullProxy);
    assert_eq!(job.pty_mode, Some(config.mode));
    cleanup_pty_tasks(&mut job).await;
    drop(scratch);

    // Downgrade: both sides agree on OutputOnly, never stale FullProxy.
    let mut downgraded = full_proxy_job();
    let downgrade_ops = FaultInjectingOps {
        fail_writer: true,
        ..FaultInjectingOps::clean()
    };
    let opened = Arc::new(AtomicBool::new(false));
    let opened_clone = Arc::clone(&opened);
    let child = setup_pty_with_ops(
        &mut downgraded,
        &mut ctx,
        recording_opener(opened_clone),
        &downgrade_ops,
    )
    .await
    .expect("setup pty");
    let config = child.expect("downgrade must return a child config");
    assert_eq!(config.mode, PtyMode::OutputOnly);
    assert_eq!(downgraded.pty_mode, Some(config.mode));
    cleanup_pty_tasks(&mut downgraded).await;
}

#[tokio::test]
async fn capture_output_task_failure_cleans_up_tasks() {
    // `capture_output_and_history` reclaims task ownership before
    // awaiting the output task, so even when that await fails no proxy
    // task or PTY fd may be left behind for the next command.
    let mut job = Job::new("pty-test".to_string(), Pid::from_raw(1));
    job.state = ProcessState::Completed(0, None);
    job.pty = Some(Pty::new().expect("failed to create test pty"));
    job.pty_mode = Some(PtyMode::OutputOnly);
    job.pty_input_task = Some(tokio::spawn(std::future::pending::<()>()));
    job.pty_output_task = Some(tokio::spawn(async {
        Err::<String, anyhow::Error>(anyhow::anyhow!("injected output failure"))
    }));

    let ctx = test_context(true, true);
    let mut shell = Shell::new(Environment::new());
    let result = capture_output_and_history(&mut job, &ctx, &mut shell).await;
    assert!(result.is_err());
    assert!(job.pty_input_task.is_none());
    assert!(job.pty_output_task.is_none());
    assert!(job.pty.is_none());
    assert!(job.pty_mode.is_none());
}

#[tokio::test]
async fn cleanup_pty_tasks_reclaims_committed_proxy_tasks() {
    // Contract test for the primitive every `launch_inner` error path
    // relies on (`CommandFailed`, launch `Err`, `manage_execution` `Err`,
    // `capture` `Err` all call `cleanup_pty_tasks`): after a committed
    // FullProxy setup, cleanup must reclaim both tasks plus PTY state.
    // A `JoinHandle` is not cancelled by `Drop`, so take + abort + await
    // here is what prevents detached proxy tasks. The end-to-end
    // `CommandFailed` path is covered by `job::tests::launch_failure`
    // style tests via real `launch`.
    let mut ctx = test_context(true, true);
    let mut job = full_proxy_job();
    let ops = FaultInjectingOps::clean();
    let scratch = Pty::new().expect("scratch pty for the input proxy");
    let scratch_fd = scratch.slave.as_raw_fd();
    let child = setup_pty_with_ops(
        &mut job,
        &mut ctx,
        move || AsyncStdin::open_tty_from_fd(unsafe { BorrowedFd::borrow_raw(scratch_fd) }),
        &ops,
    )
    .await
    .expect("setup pty");
    assert!(child.is_some());
    assert!(job.pty_input_task.is_some());
    assert!(job.pty_output_task.is_some());

    cleanup_pty_tasks(&mut job).await;

    assert!(job.pty_input_task.is_none());
    assert!(job.pty_output_task.is_none());
    assert!(job.pty.is_none());
    assert!(job.pty_mode.is_none());
    drop(scratch);
}

#[tokio::test]
async fn cleanup_pty_tasks_awaits_aborted_input_proxy() {
    let mut job = Job::new("test".to_string(), Pid::from_raw(1));
    let (started_tx, started_rx) = oneshot::channel();
    let (stopped_tx, stopped_rx) = oneshot::channel();

    job.pty_input_task = Some(tokio::spawn(async move {
        let _notify = DropNotify(Some(stopped_tx));
        let _ = started_tx.send(());
        std::future::pending::<()>().await;
    }));
    started_rx.await.expect("input proxy task started");

    cleanup_pty_tasks(&mut job).await;

    tokio::time::timeout(Duration::from_secs(1), stopped_rx)
        .await
        .expect("input proxy task was not awaited")
        .expect("input proxy task did not stop");
    assert!(job.pty_input_task.is_none());
    assert!(job.pty.is_none());
    assert!(job.pty_mode.is_none());
}

#[tokio::test]
async fn capture_stops_pty_input_before_waiting_for_output_task() {
    let mut job = Job::new("pty-test".to_string(), Pid::from_raw(1));
    job.state = ProcessState::Completed(0, None);
    job.pty = Some(Pty::new().expect("failed to create test pty"));
    job.pty_mode = Some(PtyMode::OutputOnly);

    let (started_tx, started_rx) = oneshot::channel();
    let (stopped_tx, stopped_rx) = oneshot::channel();

    job.pty_input_task = Some(tokio::spawn(async move {
        let _notify = DropNotify(Some(stopped_tx));
        let _ = started_tx.send(());
        std::future::pending::<()>().await;
    }));
    started_rx.await.expect("input proxy task started");

    job.pty_output_task = Some(tokio::spawn(async move {
        stopped_rx
            .await
            .expect("output task waited for input proxy stop");
        Ok("late pty output".to_string())
    }));

    let ctx = test_context(true, true);
    let mut shell = Shell::new(Environment::new());

    tokio::time::timeout(
        Duration::from_secs(1),
        capture_output_and_history(&mut job, &ctx, &mut shell),
    )
    .await
    .expect("capture waited for output before stopping input proxy")
    .expect("capture output and history");

    assert!(job.pty_input_task.is_none());
    assert!(job.pty_output_task.is_none());
    assert!(job.pty.is_none());
    assert!(job.pty_mode.is_none());
    assert_eq!(
        shell
            .environment
            .read()
            .session_output_state
            .output_history
            .last_stdout(),
        Some("late pty output")
    );
}
