use super::job_process::JobProcess;
use anyhow::Result;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tracing::debug;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Both tests below install handlers into one process-global SIGINT
    /// disposition and deliver a real SIGINT to this process, so concurrent
    /// execution would race each other's installs. Serialize them: the
    /// handler that intercepts a delivery must be the one the test just
    /// installed.
    static HANDLER_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Poll the latch until it reports the delivered SIGINT (or run out of
    /// retries): the SIGINT → handler → latch hop is asynchronous and the
    /// condition, not a fixed sleep, defines the wait.
    fn latch_reports_interrupt() -> bool {
        for _ in 0..100 {
            if check_and_clear_sigint() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn deliver_process_sigint() {
        let pid = Pid::from_raw(std::process::id() as i32);
        send_signal_allow_gone(pid, Signal::SIGINT).expect("self SIGINT");
    }

    /// Creating any tokio signal future (ghost text, `doctor ai`, the OAuth
    /// wait, ...) hands SIGINT to tokio, whose driver registers the signal
    /// through signal-hook-registry. That crate's handler takes over the
    /// disposition and, on every delivery, re-runs the slot's previously
    /// installed handler before tokio's own actions - so the shell's C
    /// handler keeps setting the cancel latch even under the takeover. This
    /// works only while signal-hook-registry chains the previous handler;
    /// if this test ever fails after a tokio/signal-hook-registry upgrade,
    /// product code needs a real tokio→latch bridge instead.
    #[test]
    fn tokio_signal_registration_still_reaches_the_cancel_latch() {
        let _guard = HANDLER_TEST_LOCK.lock().expect("lock poisoned");

        // The shell installs the C handler first (bootstrap, `set_signals`).
        install_sigint_handler().expect("C handler installation");

        // Creating the tokio signal future here is what swaps in tokio's
        // handler; `dsh_openai::auth::run` only block_on's it on the shared
        // runtime. Hold the future for the registration and let the swap
        // settle before the delivery.
        let banner = dsh_openai::auth::run(async {
            let _signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .expect("tokio registration");
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(())
        });
        banner.expect("openai shared runtime");

        assert!(
            !check_and_clear_sigint(),
            "latch must be clean before the delivery"
        );

        deliver_process_sigint();
        assert!(
            latch_reports_interrupt(),
            "SIGINT did not reach the cancel latch after tokio registered its own handler"
        );
    }

    /// `set_signals` re-installs the C handler at every `!` turn start, on
    /// top of whatever tokio registered earlier: `sigaction` replaces the
    /// registry's handler, so a delivery now runs `handle_sigint` directly.
    /// The flip back around must keep feeding the same latch.
    #[test]
    fn reinstalled_c_handler_remains_the_interrupt_source() {
        let _guard = HANDLER_TEST_LOCK.lock().expect("lock poisoned");

        // Tokio registers first: some earlier AI feature created a signal
        // future before this turn started.
        let banner = dsh_openai::auth::run(async {
            let _signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .expect("tokio registration");
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(())
        });
        banner.expect("openai shared runtime");

        // The per-turn reinstall that `eval` runs before the chat.
        install_sigint_handler().expect("C handler reinstallation");

        assert!(
            !check_and_clear_sigint(),
            "latch must be clean before the delivery"
        );

        deliver_process_sigint();
        assert!(
            latch_reports_interrupt(),
            "SIGINT did not reach the cancel latch through the reinstalled C handler"
        );
    }
}
