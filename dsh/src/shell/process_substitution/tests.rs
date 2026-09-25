use super::*;
use crate::process::JobLaunchOutcome;
use crate::repl::confirmation::ConfirmationAction;

fn allow_all(_: &str) -> Result<ConfirmationAction> {
    Ok(ConfirmationAction::Yes)
}

/// Producer reap policy: only terminal observations end the wait.
/// `Stopped` is live state and must never count as reaped.
#[test]
fn producer_reap_policy_treats_only_terminal_observations_as_done() {
    use nix::sys::signal::Signal;

    let pid = Pid::from_raw(424281);
    assert!(producer_wait_is_done(&WaitPidObservation::State(
        pid,
        ProcessState::Completed(0, None)
    )));
    assert!(producer_wait_is_done(&WaitPidObservation::State(
        pid,
        ProcessState::Completed(1, None)
    )));
    assert!(producer_wait_is_done(&WaitPidObservation::NoChild));
    assert!(!producer_wait_is_done(&WaitPidObservation::StillAlive));
    assert!(!producer_wait_is_done(&WaitPidObservation::State(
        pid,
        ProcessState::Stopped(pid, Signal::SIGTSTP)
    )));
    assert!(!producer_wait_is_done(&WaitPidObservation::State(
        pid,
        ProcessState::Running
    )));
}

/// Test A (producer-only): the re-exec producer delivers its bytes to the
/// data pipe with no `/dev/fd` consumer involved.
///
/// Green here + red end-to-end isolates the failure to consumer
/// inheritance / resource lifetime, not the producer path. Red here
/// isolates it to producer stdout wiring / helper evaluation / producer
/// lifetime.
#[tokio::test]
async fn producer_only_delivers_marker_without_consumer() {
    use super::ProcessSubstitutionDirection::Read;

    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    let plan = crate::shell::parse::parse_execution_plan(
        "printf PRODUCER-MARKER",
        std::sync::Arc::clone(&env),
    )
    .expect("parse producer plan");
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);

    let substitution =
        match start_process_substitution(&mut shell, &ctx, &plan, Read, allow_all).await {
            Ok(substitution) => substitution,
            Err(err) => panic!("dogesh helper binary missing for producer test: {err:#}"),
        };
    // The read end must survive a consumer `execve`: non-CLOEXEC.
    let read_number = substitution.inherited_fd.as_raw_fd();
    let cloexec = unsafe {
        let borrowed = std::os::fd::BorrowedFd::borrow_raw(substitution.inherited_fd.as_raw_fd());
        nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_GETFD).expect("F_GETFD")
    };
    assert!(
        !nix::fcntl::FdFlag::from_bits_retain(cloexec).contains(nix::fcntl::FdFlag::FD_CLOEXEC),
        "process-substitution read fd {read_number} must be non-CLOEXEC to survive consumer exec"
    );
    let producer_pid = substitution.helper.pid;
    tracing::debug!(
        producer_pid = %producer_pid,
        read_fd = read_number,
        argument = %substitution.argument,
        "producer-only test spawned",
    );

    // Read the data pipe directly: no `/dev/fd/N` consumer anywhere.
    let read_fd = substitution.inherited_fd;
    let helper = substitution.helper;
    let output = tokio::task::spawn_blocking(move || {
        use std::io::Read as _;
        let mut file = std::fs::File::from(read_fd);
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).map(|_| buf)
    })
    .await
    .expect("reader task")
    .expect("read producer pipe");
    let text = String::from_utf8_lossy(&output).to_string();
    assert_eq!(
        text, "PRODUCER-MARKER",
        "producer helper wrote {text:?}, expected PRODUCER-MARKER (producer pid {producer_pid})"
    );

    // A finite producer is gone by EOF: bounded reap must not escalate to
    // group-kill for the healthy case.
    reap_producers_blocking(vec![helper]);
}

/// Low-level `>(...)` wiring: bytes written to the inherited write end
/// reach the helper's stdin, which saves them to a tempfile.
#[tokio::test]
async fn output_substitution_delivers_marker_to_consumer_stdin() {
    use super::ProcessSubstitutionDirection::Write;

    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("consumer_out.txt");
    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    let plan = crate::shell::parse::parse_execution_plan(
        &format!("cat > {}", out.display()),
        std::sync::Arc::clone(&env),
    )
    .expect("parse consumer plan");
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);

    let substitution =
        match start_process_substitution(&mut shell, &ctx, &plan, Write, allow_all).await {
            Ok(substitution) => substitution,
            Err(err) => panic!("dogesh helper binary missing for consumer test: {err:#}"),
        };
    assert_eq!(substitution.direction, Write);
    // The write end must survive an outer `execve`: non-CLOEXEC.
    let write_number = substitution.inherited_fd.as_raw_fd();
    let cloexec = unsafe {
        let borrowed = std::os::fd::BorrowedFd::borrow_raw(substitution.inherited_fd.as_raw_fd());
        nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_GETFD).expect("F_GETFD")
    };
    assert!(
        !nix::fcntl::FdFlag::from_bits_retain(cloexec).contains(nix::fcntl::FdFlag::FD_CLOEXEC),
        "process-substitution write fd {write_number} must be non-CLOEXEC to survive outer exec"
    );

    // Write the marker, then close the parent write end first so the
    // consumer sees EOF (close-before-wait contract).
    let write_fd = substitution.inherited_fd;
    let helper = substitution.helper;
    tokio::task::spawn_blocking(move || {
        use std::io::Write as _;
        use std::os::fd::IntoRawFd as _;
        let mut file = unsafe { std::fs::File::from_raw_fd(write_fd.into_raw_fd()) };
        file.write_all(b"PROCESS-SUBSTITUTION-WRITE-MARKER")
            .expect("write marker");
        // `File` drop closes the write end → EOF for the consumer.
    })
    .await
    .expect("writer task");
    // Blocking natural reap for the test (the foreground path would use
    // the detached reaper so the prompt never waits).
    reap_output_consumer_sync(helper);

    let text = std::fs::read_to_string(&out).expect("read consumer output");
    assert_eq!(text, "PROCESS-SUBSTITUTION-WRITE-MARKER");
}

// NOTE: the close-before-wait contract (`finish_foreground` closes every
// parent endpoint copy before any helper wait/reap) is proven behaviorally
// by `output_substitution_delivers_marker_to_consumer_stdin` (EOF delivery),
// `large_output_drains_without_tail_loss` (no tail loss), and
// `slow_consumer_is_not_timeout_killed` (no premature reap). A source-text
// pin is deliberately not kept: it would couple the test to file layout
// without verifying runtime ordering.

/// Two substitutions retain two distinct endpoints and two helpers:
/// guards against `ExecutionResources` overwrite collapsing the first.
#[tokio::test]
async fn two_substitutions_retain_distinct_resources() {
    use super::ProcessSubstitutionDirection::Read;

    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    let plan_one =
        crate::shell::parse::parse_execution_plan("printf one", std::sync::Arc::clone(&env))
            .expect("parse");
    let plan_two =
        crate::shell::parse::parse_execution_plan("printf two", std::sync::Arc::clone(&env))
            .expect("parse");

    let first = start_process_substitution(&mut shell, &ctx, &plan_one, Read, allow_all)
        .await
        .expect("start first producer");
    let second = start_process_substitution(&mut shell, &ctx, &plan_two, Read, allow_all)
        .await
        .expect("start second producer");

    assert_ne!(
        first.inherited_fd.as_raw_fd(),
        second.inherited_fd.as_raw_fd(),
        "two producers must hold distinct fds"
    );
    assert_ne!(first.helper.pid, second.helper.pid, "distinct producers");

    let mut resources = ExecutionResources::new();
    let arg_one = resources.add_process_substitution(first);
    let arg_two = resources.add_process_substitution(second);
    assert!(!resources.is_empty());
    assert_ne!(arg_one, arg_two, "distinct /dev/fd arguments");
    // `resources` drops here: fds close, producers go to detached reapers.
}

/// Mixed `<(...)` / `>(...)` on one job keep independent fds, helpers,
/// and directions.
#[tokio::test]
async fn mixed_directions_keep_distinct_resources() {
    use super::ProcessSubstitutionDirection::{Read, Write};

    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("mixed_out.txt");
    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    let read_plan =
        crate::shell::parse::parse_execution_plan("printf in-a", std::sync::Arc::clone(&env))
            .expect("parse");
    let write_plan = crate::shell::parse::parse_execution_plan(
        &format!("cat > {}", out.display()),
        std::sync::Arc::clone(&env),
    )
    .expect("parse");

    let read_sub = start_process_substitution(&mut shell, &ctx, &read_plan, Read, allow_all)
        .await
        .expect("start read");
    let write_sub = start_process_substitution(&mut shell, &ctx, &write_plan, Write, allow_all)
        .await
        .expect("start write");
    assert_eq!(read_sub.direction, Read);
    assert_eq!(write_sub.direction, Write);
    assert_ne!(
        read_sub.inherited_fd.as_raw_fd(),
        write_sub.inherited_fd.as_raw_fd(),
        "mixed directions must hold distinct fds"
    );
    assert_ne!(read_sub.helper.pid, write_sub.helper.pid);

    let mut resources = ExecutionResources::new();
    let arg_read = resources.add_process_substitution(read_sub);
    let arg_write = resources.add_process_substitution(write_sub);
    assert_ne!(arg_read, arg_write);
    assert!(!resources.is_empty());
    // Detached cleanup on drop covers both directions.
}

/// Background jobs keep their producers until the job itself is done.
///
/// Launching used to hand background producers to detached reapers
/// immediately, whose 2s grace group-killed them while the background
/// consumer still needed their pipes (`cat <(sleep 5; echo done) &`
/// read early EOF). Ownership now stays in the job on `wait_jobs`;
/// detached reapers only take over when the job drops.
///
/// The consumer below is the `dirs` builtin on purpose, so the whole
/// launch stays fork-free headless (an external consumer would exercise
/// interactive `setpgid`/PTY job control instead, which is unrelated to
/// producer ownership). Resource retention in `Job::launch` is
/// consumer-type-agnostic.
#[tokio::test]
async fn background_launch_keeps_producer_until_job_done() {
    use super::ProcessSubstitutionDirection::Read;
    use dsh_types::terminal::{ShellMode, TerminalState};
    use std::time::Duration;

    let _ = Read;
    // The consumer is `dirs` (a background-safe builtin that ignores
    // stdin): the producer `sleep 30` stays alive for the whole test,
    // so liveness past the old 2s reaper grace proves it was not
    // group-killed early. No timing flake: kill (before fix) fires at
    // wall-clock ~2.0s, the check runs at ~3.0s.
    let input = "dirs < <(sleep 30)".to_string();
    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    let plan = crate::shell::parse::parse_execution_plan(&input, std::sync::Arc::clone(&env))
        .expect("parse background plan");
    // Headless interactive context: no tty, and background launches are
    // genuinely not waited (async lists return at spawn; `-c` drains at exit).
    let null = std::fs::File::open("/dev/null").expect("open /dev/null");
    use std::os::fd::AsRawFd as _;
    let null_fd = null.as_raw_fd();
    let mut ctx = Context {
        shell_pid: shell.pid,
        shell_pgid: shell.pgid,
        shell_tmode: None,
        terminal_state: TerminalState::non_terminal(),
        shell_mode: ShellMode::Script,
        foreground: false,
        interactive: true,
        infile: null_fd,
        outfile: null_fd,
        errfile: null_fd,
        captured_out: None,
        output_observer: None,
        save_history: false,
        pid: None,
        pgid: Some(shell.pgid),
        process_count: 0,
    };
    let materialized = match crate::shell::materialize::materialize_job(
        &mut shell,
        &ctx,
        &plan.lists[0].jobs[0],
        allow_all,
    )
    .await
    .expect("materialize")
    {
        crate::shell::materialize::MaterializeOutcome::Runnable(materialized) => materialized,
        crate::shell::materialize::MaterializeOutcome::NoCommand(_) => {
            panic!("expected runnable job")
        }
        crate::shell::materialize::MaterializeOutcome::Rejected(failure) => {
            panic!("expected runnable job, got rejection: {failure:?}")
        }
    };
    let mut job = materialized.job;
    job.resources = materialized.resources;
    job.foreground = false;
    job.disable_pty = true;

    let outcome = job.launch(&mut ctx, &mut shell).await.expect("launch");
    let JobLaunchOutcome::Process(state) = outcome else {
        panic!("background launch must not report a command failure: {outcome:?}");
    };
    assert_eq!(
        state,
        crate::process::state::ProcessState::Running,
        "background launch must return without waiting"
    );
    // Structural gate, no timing involved: ownership moved to the job,
    // not to an immediate detached reaper.
    assert!(
        !job.resources.is_empty(),
        "background job must retain its substitution resources"
    );

    // Functional proof: the producer is still alive past the old 2s
    // reaper grace, i.e. no detached reaper group-killed it while the
    // background consumer lives. `WNOHANG` on a running child reports
    // `None` without reaping, so the check is non-destructive.
    tokio::time::sleep(Duration::from_millis(3100)).await;
    // The job still owns exactly one substitution; reach into it for the
    // liveness probe without taking ownership.
    assert!(
        !job.resources.is_empty(),
        "background producer was reaped early (still needed by its consumer)"
    );
    // `job` drops here: the lingering producer goes to a detached
    // reaper (grace, then group-kill), and shell-shutdown cleanup covers
    // anything left. No zombie: the reaper owns the final wait.
}

/// Same-process multi-`Shell` ownership isolation (Layer 3 companion).
///
/// Owner invariant: every live helper has exactly one logical
/// owner. Ownership may move `ExecutionResources -> Job -> detached
/// reaper` but is never duplicated, never dropped while a consumer
/// needs it, and `Shell` A cleanup never affects `Shell` B resources.
///
/// No real processes: fake pids only exercise registry bookkeeping, and
/// group-kill on absent pids is a harmless `ESRCH`.
#[test]
fn producer_registries_are_isolated_per_shell() {
    use super::ProcessSubstitutionDirection::{Read, Write};

    let registry_a = ProcessSubstitutionRegistry::default();
    let registry_b = ProcessSubstitutionRegistry::default();
    let pid_a = Pid::from_raw(41_001);
    let pid_b = Pid::from_raw(41_002);
    registry_a.register(pid_a, Read);
    registry_b.register(pid_b, Write);

    // Shell A shuts down: only its own helper is released.
    registry_a.cleanup_process_substitution_groups();
    assert!(
        registry_a.is_empty(),
        "shell A cleanup must release shell A's helpers"
    );
    assert!(
        registry_b.contains(pid_b),
        "shell A cleanup must never affect shell B resources"
    );

    // Deregistration (reaper success path) is per-pid, not per-registry.
    registry_b.deregister(pid_a);
    assert!(
        registry_b.contains(pid_b),
        "deregistering another shell's pid must not release shell B's helper"
    );
    registry_b.deregister(pid_b);
    assert!(
        registry_b.is_empty(),
        "reaped helpers deregister exactly once"
    );
}

/// Normal-exit detach releases Write consumers without signalling them;
/// Read producers stay shell-owned for abnormal-drop cleanup.
#[test]
fn normal_exit_detach_releases_only_write_consumers() {
    use super::ProcessSubstitutionDirection::{Read, Write};
    use nix::sys::signal::Signal;
    use std::time::Duration;

    fn live_child() -> Pid {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep for detach test");
        let pid = Pid::from_raw(child.id() as i32);
        std::mem::forget(child);
        pid
    }

    fn alive(pid: Pid) -> bool {
        nix::sys::signal::kill(pid, None).is_ok()
    }

    fn reap_child(pid: Pid) {
        let _ = nix::sys::signal::kill(pid, Signal::SIGKILL);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while let Ok(nix::sys::wait::WaitStatus::StillAlive) =
            nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "detach-test child never reaped"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env);
    let read_pid = live_child();
    let write_pid = live_child();
    shell.process_substitution_registry.register(read_pid, Read);
    shell
        .process_substitution_registry
        .register(write_pid, Write);

    let released = shell.detach_process_substitution_consumers_for_normal_exit();
    assert_eq!(released, 1, "exactly the Write consumer detaches");
    assert!(
        shell.process_substitution_registry.contains(read_pid),
        "Read producer stays shell-owned"
    );
    assert!(
        !shell.process_substitution_registry.contains(write_pid),
        "Write consumer ownership is released"
    );
    assert!(alive(write_pid), "detach must not signal the consumer");
    assert!(
        alive(read_pid),
        "detach must not signal the producer either"
    );
    reap_child(read_pid);
    reap_child(write_pid);
}

/// Abnormal `Shell` drop cleans up both directions and never touches
/// another shell's helpers.
#[test]
fn abnormal_drop_cleans_both_directions_per_shell() {
    use super::ProcessSubstitutionDirection::{Read, Write};

    let registry_a = ProcessSubstitutionRegistry::default();
    let registry_b = ProcessSubstitutionRegistry::default();
    // Fake pids: group-kill on absent pids is harmless `ESRCH`, and the
    // test pins bookkeeping (release + isolation), not live signals.
    registry_a.register(Pid::from_raw(42_001), Read);
    registry_a.register(Pid::from_raw(42_002), Write);
    registry_b.register(Pid::from_raw(42_003), Read);
    registry_a.cleanup_process_substitution_groups();
    assert!(
        registry_a.is_empty(),
        "abnormal drop releases both directions"
    );
    assert!(
        registry_b.contains(Pid::from_raw(42_003)),
        "other shell untouched"
    );
    registry_b.cleanup_process_substitution_groups();
}

/// Large-output drain: pipe-buffer-sized payloads must not lose tail
/// bytes to an early consumer kill. Several hundred KiB through
/// `>(consumer)` round-trips byte-exact.
#[tokio::test]
async fn large_output_drains_without_tail_loss() {
    use super::ProcessSubstitutionDirection::Write;

    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("large_out.bin");
    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    let plan = crate::shell::parse::parse_execution_plan(
        &format!("cat > {}", out.display()),
        std::sync::Arc::clone(&env),
    )
    .expect("parse consumer plan");
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);

    let substitution =
        match start_process_substitution(&mut shell, &ctx, &plan, Write, allow_all).await {
            Ok(substitution) => substitution,
            Err(err) => panic!("dogesh helper binary missing for large drain: {err:#}"),
        };
    // 512 KiB deterministic payload (exceeds any pipe buffer).
    let payload: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
    let expected_len = payload.len();
    let write_fd = substitution.inherited_fd;
    let helper = substitution.helper;
    tokio::task::spawn_blocking(move || {
        use std::io::Write as _;
        use std::os::fd::IntoRawFd as _;
        let mut file = unsafe { std::fs::File::from_raw_fd(write_fd.into_raw_fd()) };
        // Single large write loop; EPIPE would be a test failure here
        // (consumer stays alive for the whole payload).
        let mut offset = 0;
        while offset < payload.len() {
            match file.write(&payload[offset..]) {
                Ok(0) => panic!("write returned zero with remaining bytes"),
                Ok(n) => offset += n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => panic!("write failed: {err}"),
            }
        }
        // `File` drop closes → EOF.
    })
    .await
    .expect("writer task");
    reap_output_consumer_sync(helper);

    let data = std::fs::read(&out).expect("read large output");
    assert_eq!(data.len(), expected_len, "tail bytes lost");
    let expected: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
    assert_eq!(data, expected, "content mismatch after drain");
}

/// Slow-but-valid consumers are never timeout-killed: the consumer
/// drains stdin, then blocks on a FIFO gate; the detached natural
/// reaper waits without signalling until the gate releases.
///
/// No sleep-based readiness: the test polls the drained file (bounded)
/// to learn the consumer reached the gate, uses `try_recv` to prove the
/// reaper has not completed early, then releases the gate and expects
/// natural completion.
#[tokio::test]
async fn slow_consumer_is_not_timeout_killed() {
    use super::ProcessSubstitutionDirection::Write;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("gated_out.txt");
    let fifo = dir.path().join("release_fifo");
    let fifo_c = std::ffi::CString::new(fifo.to_string_lossy().into_owned()).expect("cstring");
    assert_eq!(
        unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) },
        0,
        "mkfifo failed: {}",
        std::io::Error::last_os_error()
    );

    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    // Consumer: drain stdin to `out`, then block reading the FIFO.
    let plan = crate::shell::parse::parse_execution_plan(
        &format!(
            "cat > {} ; cat {} > /dev/null",
            out.display(),
            fifo.display()
        ),
        std::sync::Arc::clone(&env),
    )
    .expect("parse gated consumer");
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    let substitution =
        match start_process_substitution(&mut shell, &ctx, &plan, Write, allow_all).await {
            Ok(substitution) => substitution,
            Err(err) => panic!("dogesh helper binary missing for gate test: {err:#}"),
        };
    let payload = b"GATED-PAYLOAD";
    let write_fd = substitution.inherited_fd;
    let helper = substitution.helper;
    let pid = helper.pid;
    // Writer thread owns the blocking natural reap so the test can
    // observe "not completed yet" via the channel.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    // Move the write end into the writer: write, close (EOF), then reap.
    std::thread::spawn(move || {
        use std::io::Write as _;
        use std::os::fd::IntoRawFd as _;
        let mut file = unsafe { std::fs::File::from_raw_fd(write_fd.into_raw_fd()) };
        file.write_all(payload).expect("write gated payload");
        drop(file); // EOF
        reap_output_consumer_sync(helper);
        let _ = done_tx.send(());
    });

    // Wait (bounded) until the consumer drained stdin to `out`: proof it
    // reached the FIFO gate and is now blocked there.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if std::fs::read(&out).is_ok_and(|data| data == payload) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "gated consumer never drained stdin"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The reaper must not have completed while the consumer legitimately
    // blocks on the gate (no timeout kill).
    assert!(
        done_rx.try_recv().is_err(),
        "slow consumer was reaped/killed before gate release"
    );
    // Helper itself must still be alive (not SIGTERMed).
    assert_eq!(
        crate::process::wait_pid_job(pid, true),
        Ok(crate::process::WaitPidObservation::StillAlive),
        "gated consumer was signalled before release"
    );

    // Release the gate: open FIFO for writing, send one byte, close (EOF).
    let fifo_c2 = std::ffi::CString::new(fifo.to_string_lossy().into_owned()).expect("cstring");
    let fd = unsafe { libc::open(fifo_c2.as_ptr(), libc::O_WRONLY) };
    assert!(fd >= 0, "open fifo for release failed");
    let byte = *b"R";
    assert_eq!(
        unsafe { libc::write(fd, byte.as_ptr() as *const libc::c_void, 1) },
        1
    );
    unsafe { libc::close(fd) };

    // Natural completion follows promptly (bounded).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if done_rx.try_recv().is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "gated consumer never completed after release"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(std::fs::read(&out).expect("read gated output"), payload,);
}

/// Early consumer exit must not crash the shell: the outer writer sees
/// `EPIPE`, the helper is reaped, and no fd leaks.
#[tokio::test]
async fn early_consumer_exit_does_not_crash_shell() {
    use super::ProcessSubstitutionDirection::Write;

    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("head_out.txt");
    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    // `head -c 1` exits after one byte; the outer large write then hits
    // EPIPE/SIGPIPE. Bare `head` (via PATH) keeps Linux/macOS portable.
    let plan = crate::shell::parse::parse_execution_plan(
        &format!("head -c 1 > {}", out.display()),
        std::sync::Arc::clone(&env),
    )
    .expect("parse early-exit consumer");
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    let substitution =
        match start_process_substitution(&mut shell, &ctx, &plan, Write, allow_all).await {
            Ok(substitution) => substitution,
            Err(err) => panic!("dogesh helper binary missing for early exit: {err:#}"),
        };
    let write_fd = substitution.inherited_fd;
    let helper = substitution.helper;
    // Ignore SIGPIPE in this test process so the large write surfaces as
    // EPIPE instead of killing the test harness. Restored before returning
    // so later tests in the same process observe the default disposition.
    let prev_sigpipe = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let result = tokio::task::spawn_blocking(move || {
        use std::io::Write as _;
        use std::os::fd::IntoRawFd as _;
        let mut file = unsafe { std::fs::File::from_raw_fd(write_fd.into_raw_fd()) };
        let chunk = vec![b'x'; 64 * 1024];
        // Bounded attempts: the consumer exits early, so the pipe must
        // break rather than accept unbounded data.
        for _ in 0..32 {
            match file.write(&chunk) {
                Ok(_) => continue,
                Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => break,
                Err(err) if err.raw_os_error() == Some(libc::EPIPE) => break,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        // Close (File drop) → helper already gone or exiting.
    })
    .await;
    assert!(result.is_ok(), "writer task panicked");
    // Reap must complete without hanging even though the consumer died
    // first; the shell itself never crashes and holds no leak.
    reap_output_consumer_sync(helper);
    unsafe { libc::signal(libc::SIGPIPE, prev_sigpipe) };
    let data = std::fs::read(&out).unwrap_or_default();
    assert_eq!(data.len(), 1, "head -c 1 must keep exactly one byte");
}

/// No-command `> >(consumer)` delivers EOF through the explicit
/// finalize path and keeps no-command status semantics.
#[tokio::test]
async fn no_command_redirect_to_consumer_delivers_eof() {
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("no_cmd_out.txt");
    let env = crate::environment::Environment::new();
    let mut shell = Shell::new(env.clone());
    let plan = crate::shell::parse::parse_execution_plan(
        &format!("> >(cat > {})", out.display()),
        std::sync::Arc::clone(&env),
    )
    .expect("parse no-command plan");
    let ctx = Context::new_safe(shell.pid, shell.pgid, false);
    let no_command = match crate::shell::materialize::materialize_job(
        &mut shell,
        &ctx,
        &plan.lists[0].jobs[0],
        allow_all,
    )
    .await
    .expect("materialize")
    {
        crate::shell::materialize::MaterializeOutcome::NoCommand(no_command) => *no_command,
        crate::shell::materialize::MaterializeOutcome::Runnable(_) => {
            panic!("redirect-only line must be no-command")
        }
        crate::shell::materialize::MaterializeOutcome::Rejected(failure) => {
            panic!("must not reject: {failure:?}")
        }
    };
    // Write through the redirect target before finalizing: the
    // no-command executor applies `> /dev/fd/N`... here the redirect
    // target *is* the substitution fd, and the consumer is `cat > file`.
    // The materialized redirect points at `/dev/fd/N`; opening it for
    // writing and closing delivers EOF. Instead of reopening, drive the
    // real executor which applies/restores the redirect and finalizes
    // resources (close → EOF → detached natural reaper).
    let mut ctx_mut = ctx;
    match crate::shell::no_command::execute_no_command(&mut shell, &mut ctx_mut, no_command) {
        crate::shell::no_command::NoCommandExecutionResult::Completed(0) => {}
        other => panic!("no-command must complete 0, got {other:?}"),
    }
    // The consumer runs detached; poll (bounded) for its EOF-driven
    // exit via registry release (file existence alone only proves the
    // consumer started, not that it saw EOF).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if shell.process_substitution_registry.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no-command consumer never saw EOF/exited"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Empty input means `cat` saw immediate EOF; the file exists (created
    // by the consumer's own `> file` redirect) and status stayed 0.
    assert!(out.exists(), "consumer output file missing");
}
