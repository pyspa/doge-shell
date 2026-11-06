use super::*;

/// An inherited auxiliary fd referenced as `/dev/fd/N` survives an
/// internal helper spawn: the protocol's dynamically reserved targets
/// must never overwrite it.
#[test]
fn plan_helper_preserves_inherited_aux_fd() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    // Auxiliary pipe in the `<(...)` shape: non-CLOEXEC read end, so the
    // helper inherits it across `posix_spawn` like a consumer would.
    let mut aux = [0 as RawFd; 2];
    assert_eq!(unsafe { libc::pipe(aux.as_mut_ptr()) }, 0, "pipe failed");
    // SAFETY: `pipe` succeeded, so both fds are owned exactly once.
    let aux_read = unsafe { OwnedFd::from_raw_fd(aux[0]) };
    let mut aux_write = unsafe { std::fs::File::from_raw_fd(aux[1]) };
    aux_write
        .write_all(b"FD-COLLISION-MARKER")
        .expect("write marker");
    drop(aux_write);
    let aux_number = aux_read.as_raw_fd();

    // `/bin/cat` exists on both Linux and macOS (unlike some other
    // coreutils, which live only under `/usr/bin` on macOS).
    let cat = "/bin/cat";
    let env_arc = crate::environment::Environment::new();
    let plan = crate::shell::parse::parse_execution_plan(
        &format!("{cat} /dev/fd/{aux_number}"),
        env_arc.clone(),
    )
    .expect("parse helper plan");
    let snapshot = ChildShellSnapshot::capture(&env_arc.read());

    let (cap_read, cap_write) = crate::process::io::cloexec_pipe().expect("capture pipe");
    let (status_read, status_write) = crate::process::io::cloexec_pipe().expect("status pipe");
    let null_in = std::fs::File::open("/dev/null").expect("open /dev/null");
    let null_err = std::fs::File::open("/dev/null").expect("open /dev/null");
    let producer = match spawn_plan_helper(
        &snapshot,
        &plan,
        PlanExecMode::CommandSubstitution,
        PlanSignalPolicy::Normal,
        ChildStdio {
            stdin: null_in.as_raw_fd(),
            stdout: cap_write.as_raw_fd(),
            stderr: null_err.as_raw_fd(),
        },
        Pid::from_raw(0),
        Some(status_write.as_raw_fd()),
    ) {
        Ok(pid) => pid,
        // No silent skip: a missing helper binary means the test
        // invocation never built `dogesh` (run `cargo test -p doge-shell`
        // so the sibling binary exists). Passing vacuously would hide
        // the fd-collision regression this test guards.
        Err(err) => panic!("dogesh helper binary missing for re-exec test: {err:#}"),
    };
    drop(cap_write);
    drop(status_write);

    let mut output = Vec::new();
    std::fs::File::from(cap_read)
        .read_to_end(&mut output)
        .expect("read helper output");
    let _ = crate::process::wait_pid_job(producer, false);
    let mut verdict = [0u8; 1];
    let verdict_len = std::fs::File::from(status_read)
        .read(&mut verdict)
        .unwrap_or(0);
    assert_eq!(verdict_len, 1, "helper must report one status byte");
    assert_eq!(verdict[0], b'A', "helper plan must run to completion");
    assert_eq!(
        String::from_utf8_lossy(&output),
        "FD-COLLISION-MARKER",
        "aux fd /dev/fd/{aux_number} must survive the helper spawn"
    );
}

#[test]
fn background_spawn_refuses_session_bound_builtins() {
    use nix::unistd::Pid;
    let env_arc = crate::environment::Environment::new();
    let mut shell = Shell::new(env_arc);
    let mut ctx = Context::new_safe(Pid::from_raw(1), Pid::from_raw(1), false);
    let handler = dsh_builtin::get_handler("jobs").expect("jobs builtin");
    let mut process =
        BuiltinProcess::new_handler("jobs".to_string(), handler, vec!["jobs".to_string()]);
    let err = spawn_background_builtin(&mut ctx, &mut process, &mut shell)
        .expect_err("jobs must not re-exec");
    assert!(err.to_string().contains("cannot run in background"));
}

#[test]
fn normal_spawn_mask_does_not_block_int_or_quit() {
    // The historical spawn posture: helpers start with an empty mask.
    // No real process is spawned; this pins the race-free structure.
    let mask = spawn_sigmask(HelperSpawnSignalPolicy::Normal);
    assert!(!mask.contains(Signal::SIGINT));
    assert!(!mask.contains(Signal::SIGQUIT));
    assert!(!mask.contains(Signal::SIGTERM));
    assert_eq!(mask, SigSet::empty());
}

#[test]
fn async_no_job_control_spawn_mask_blocks_int_and_quit() {
    // The race closure: an async no-job-control helper starts with
    // SIGINT/SIGQUIT blocked, installs `SIG_IGN`, then unblocks — so no
    // `SIG_DFL` delivery window exists after spawn. Nothing else is
    // blocked: SIGTERM in particular must stay deliverable.
    let mask = spawn_sigmask(HelperSpawnSignalPolicy::BlockInterruptAndQuitDuringInit);
    assert!(mask.contains(Signal::SIGINT));
    assert!(mask.contains(Signal::SIGQUIT));
    assert!(!mask.contains(Signal::SIGTERM));
    assert!(!mask.contains(Signal::SIGTSTP));
    assert!(!mask.contains(Signal::SIGCHLD));
}

#[test]
fn spawn_sigdefault_resets_job_control_set_without_preservation() {
    // Top-level and `Normal` spawns: the full job-control set reverts to
    // default so helpers never inherit the shell's ignored dispositions.
    let set = spawn_sigdefault(false);
    for sig in [
        Signal::SIGINT,
        Signal::SIGQUIT,
        Signal::SIGTSTP,
        Signal::SIGTTIN,
        Signal::SIGTTOU,
        Signal::SIGCHLD,
        Signal::SIGPIPE,
    ] {
        assert!(set.contains(sig), "{sig:?} must reset to default");
    }
    assert!(!set.contains(Signal::SIGTERM));
}

#[test]
fn spawn_sigdefault_preserves_int_quit_inside_async_environment() {
    // Nested helpers spawned inside an async no-job-control helper keep
    // the inherited ignore; every other disposition still resets.
    let set = spawn_sigdefault(true);
    assert!(!set.contains(Signal::SIGINT));
    assert!(!set.contains(Signal::SIGQUIT));
    for sig in [
        Signal::SIGTSTP,
        Signal::SIGTTIN,
        Signal::SIGTTOU,
        Signal::SIGCHLD,
        Signal::SIGPIPE,
    ] {
        assert!(set.contains(sig), "{sig:?} must still reset to default");
    }
}
