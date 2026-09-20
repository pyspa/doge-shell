use super::job_process::JobProcess;
use anyhow::Result;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tracing::{debug, error};

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, sigaction};
use std::sync::atomic::{AtomicBool, Ordering};

static RECEIVED_SIGINT: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigint(_: i32) {
    RECEIVED_SIGINT.store(true, Ordering::SeqCst);
}

pub(crate) fn install_sigint_handler() -> Result<()> {
    tracing::info!("🔧 SIGNAL: Installing SIGINT handler");
    let handler = SigHandler::Handler(handle_sigint);
    let action = SigAction::new(handler, SaFlags::empty(), SigSet::empty());
    unsafe {
        sigaction(Signal::SIGINT, &action)?;
    }
    // Ensure SIGINT is not blocked
    unblock_sigint()?;
    tracing::info!("🔧 SIGNAL: SIGINT handler installed and unblocked");
    Ok(())
}

fn unblock_sigint() -> Result<()> {
    let mut set = SigSet::empty();
    set.add(Signal::SIGINT);
    nix::sys::signal::sigprocmask(nix::sys::signal::SigmaskHow::SIG_UNBLOCK, Some(&set), None)?;
    Ok(())
}

pub(crate) fn check_and_clear_sigint() -> bool {
    RECEIVED_SIGINT.swap(false, Ordering::SeqCst)
}

pub(crate) fn send_signal(pid: Pid, signal: Signal) -> Result<()> {
    debug!("📡 SIGNAL: Sending signal {:?} to pid {}", signal, pid);
    match kill(pid, signal) {
        Ok(_) => {
            debug!(
                "📡 SIGNAL: Successfully sent signal {:?} to pid {}",
                signal, pid
            );
            Ok(())
        }
        Err(e) => {
            error!(
                "📡 SIGNAL: Failed to send signal {:?} to pid {}: {}",
                signal, pid, e
            );
            Err(e.into())
        }
    }
}

/// Best-effort signal where `ESRCH` (already gone) is success-equivalent.
///
/// Shutdown cleanup routinely races natural exits: `Running` was observed,
/// then the process exits on its own before `kill` runs. `EPERM`/`EINVAL`
/// and other errors are still reported to the caller.
pub(crate) fn send_signal_allow_gone(pid: Pid, signal: Signal) -> Result<()> {
    match kill(pid, signal) {
        Ok(()) => Ok(()),
        Err(nix::errno::Errno::ESRCH) => {
            debug!(
                "📡 SIGNAL: pid {} already gone (ESRCH) for signal {:?}; treating as success",
                pid, signal
            );
            Ok(())
        }
        Err(err) => {
            debug!(
                "📡 SIGNAL: Failed to send signal {:?} to pid {}: {}",
                signal, pid, err
            );
            Err(err.into())
        }
    }
}

/// Signal every owned child in the canonical pipeline tree.
///
/// Best-effort and always `Ok`: per-pid failures are logged and the walk
/// continues, so callers cannot distinguish "all signaled" from "some
/// missed" — the `Result` only exists to fit `?`-style call sites. Borrowed
/// traversal (`next_process`): no clones. Nodes without a pid,
/// in-process builtins (`pid == shell_pid`), and already-`Completed` stages
/// are skipped. `ESRCH` handling is shared with [`send_signal_allow_gone`].
/// State is never synthesized here — only the later `waitpid` observation
/// may transition the tree.
pub(crate) fn signal_process_tree(
    process: Option<&JobProcess>,
    signal: Signal,
    shell_pid: Pid,
) -> Result<()> {
    use super::state::ProcessState;

    let mut current = process;
    while let Some(node) = current {
        if let Some(pid) = node.get_pid()
            && pid != shell_pid
            && !matches!(node.get_state(), ProcessState::Completed(_, _))
        {
            match send_signal_allow_gone(pid, signal) {
                Ok(()) => {
                    debug!(
                        "📡 SIGNAL: Sent signal {:?} to pid {} ({})",
                        signal,
                        pid,
                        node.get_cmd()
                    );
                }
                Err(err) => {
                    debug!(
                        "📡 SIGNAL: Failed to send signal {:?} to pid {} ({}): {}; continuing",
                        signal,
                        pid,
                        node.get_cmd(),
                        err
                    );
                }
            }
        }
        current = node.next_process();
    }
    Ok(())
}
