use super::job_process::JobProcess;
use anyhow::Result;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tracing::{debug, error};

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
