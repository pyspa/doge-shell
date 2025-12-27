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
    // eprintln!("dsh: DEBUG: SIGINT handler installed");
    Ok(())
}

fn unblock_sigint() -> Result<()> {
    let mut set = SigSet::empty();
    set.add(Signal::SIGINT);
    nix::sys::signal::sigprocmask(nix::sys::signal::SigmaskHow::SIG_UNBLOCK, Some(&set), None)?;
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn check_and_clear_sigint() -> bool {
    // eprintln!("dsh: DEBUG: Checking SIGINT state");
    let received = RECEIVED_SIGINT.swap(false, Ordering::SeqCst);
    if received {
        // eprintln!("dsh: DEBUG: SIGINT detected and cleared!");
    }
    received
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

pub(crate) fn kill_process(process: &Option<Box<JobProcess>>) -> Result<()> {
    debug!("💀 KILL: Starting kill_process");
    if let Some(process) = process {
        debug!("💀 KILL: Killing process: {}", process.get_cmd());
        match process.kill() {
            Ok(_) => debug!(
                "💀 KILL: Successfully killed process: {}",
                process.get_cmd()
            ),
            Err(e) => error!(
                "💀 KILL: Failed to kill process {}: {}",
                process.get_cmd(),
                e
            ),
        }

        if process.next().is_some() {
            debug!("💀 KILL: Killing next process in pipeline");
            kill_process(&process.next())?;
        } else {
            debug!("💀 KILL: No next process to kill");
        }
    } else {
        debug!("💀 KILL: No process to kill");
    }
    debug!("💀 KILL: kill_process completed");
    Ok(())
}
