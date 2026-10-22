//! Failed-launch endpoint cleanup and bounded canonical child rollback.

use super::*;

impl Job {
    /// Move the complete canonical owner, including monitors/resources, onto
    /// the existing managed table. No second waiter or status is created.
    pub(super) fn handoff_failed_launch(&mut self, shell: &mut Shell) {
        let placeholder = Job::new(self.cmd.clone(), self.shell_pgid);
        shell.wait_jobs.push(std::mem::replace(self, placeholder));
    }

    /// Only a downstream stage receives an owned pipeline reader here.
    /// The first stage's input and the PTY slave belong to other owners.
    pub(super) fn close_failed_stage_input(&self, ctx: &mut Context, previous_infile: RawFd) {
        if self.process.is_some() && previous_infile != self.stdin {
            let _ = close(previous_infile);
            ctx.infile = self.stdin;
        }
    }

    /// One rollback per failed launch, using only the canonical signal/wait
    /// authorities. A timeout leaves the live tree owned for handoff by launch.
    pub(super) async fn abort_spawned_stages(&mut self) {
        if self.process.is_some() {
            let _ = self.signal(Signal::SIGKILL);
            #[cfg(test)]
            if self.defer_abort_reap {
                return;
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                self.update_status();
                if self.is_process_tree_completed() {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    debug!("abort_spawned_stages: reap timed out for '{}'", self.cmd);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
    }
}
