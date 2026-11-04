//! Readiness and retry ownership for the environment-local PATH ghost projection.
//! Scan workers never hold Environment while doing filesystem I/O.
use super::Environment;
use crate::completion::generator::{self, SystemCommandScanTicket};
use parking_lot::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

const RETRY: Duration = Duration::from_secs(1);
const REFRESH: Duration = Duration::from_secs(30);
#[derive(Debug, Default)]
pub(crate) enum Phase {
    #[default]
    Uninitialized,
    Pending,
    Ready(Instant),
    Failed(Instant),
}
#[derive(Debug, Default)]
pub(crate) struct PathGhost {
    paths: Vec<String>,
    ticket: Option<SystemCommandScanTicket>,
    pub(super) phase: Phase,
    notifier: Option<UnboundedSender<()>>,
    failures: u32,
}
impl PathGhost {
    pub(super) fn uninitialized_for(
        paths: Vec<String>,
        notifier: Option<UnboundedSender<()>>,
    ) -> Self {
        Self {
            paths,
            notifier,
            ..Self::default()
        }
    }
    pub(super) fn take_notifier(&mut self) -> Option<UnboundedSender<()>> {
        self.notifier.take()
    }
    fn failed(&mut self) {
        let delay = RETRY.saturating_mul(1 << self.failures.min(5)).min(REFRESH);
        self.failures = self.failures.saturating_add(1);
        self.phase = Phase::Failed(Instant::now() + delay);
    }
    fn needs_scan(&self, paths: &[String]) -> bool {
        self.paths != paths
            || match self.phase {
                Phase::Uninitialized => true,
                Phase::Pending => false,
                Phase::Ready(at) => at.elapsed() >= REFRESH,
                Phase::Failed(retry_at) => Instant::now() >= retry_at,
            }
    }
}
impl Environment {
    pub(crate) fn ensure_path_ghost_scan(&self) {
        if !self.invocation.interactive
            || self.isolated_projection
            || !super::paths::path_lookup_is_cacheable(&self.variable_state.paths)
        {
            return;
        }
        if self
            .completion_state
            .path_ghost
            .lock()
            .needs_scan(&self.variable_state.paths)
        {
            self.start_path_ghost_scan(super::collect_executables_checked);
        }
    }
    pub(crate) fn path_ghost_pending(&self) -> bool {
        self.invocation.interactive
            && !self.isolated_projection
            && super::paths::path_lookup_is_cacheable(&self.variable_state.paths)
            && matches!(
                self.completion_state.path_ghost.lock().phase,
                Phase::Pending
            )
            && self.completion_state.executable_names.read().is_empty()
    }
    pub(crate) fn set_path_ghost_notifier(&self, sender: UnboundedSender<()>) {
        let settled = {
            let mut state = self.completion_state.path_ghost.lock();
            state.notifier = Some(sender.clone());
            matches!(state.phase, Phase::Ready(_) | Phase::Failed(_))
        };
        // A worker can finish before Repl::new installs this subscription.
        if settled {
            let _ = sender.send(());
        }
        self.ensure_path_ghost_scan();
    }
    pub(super) fn mark_path_ghost_ready(&self) {
        let mut state = self.completion_state.path_ghost.lock();
        state.paths = self.variable_state.paths.clone();
        state.ticket = None;
        state.phase = Phase::Ready(Instant::now());
        state.failures = 0;
    }
    pub(crate) fn start_path_ghost_scan<L>(&self, loader: L) -> Option<std::thread::JoinHandle<()>>
    where
        L: FnOnce(&[String]) -> anyhow::Result<Vec<String>> + Send + 'static,
    {
        if self.isolated_projection
            || !super::paths::path_lookup_is_cacheable(&self.variable_state.paths)
        {
            return None;
        }
        let paths = self.variable_state.paths.clone();
        let activation = generator::activate_system_command_cache(&paths);
        let ticket = generator::begin_background_system_command_scan(&activation);
        let projection = self.completion_state.path_ghost.clone();
        let names = self.completion_state.executable_names.clone();
        {
            let mut state = projection.lock();
            if state
                .ticket
                .as_ref()
                .is_some_and(|old| old.order_key() > ticket.order_key())
            {
                return None;
            }
            if state.paths != paths {
                names.write().clear();
                state.failures = 0;
            }
            state.paths = paths.clone();
            state.ticket = Some(ticket.clone());
            state.phase = Phase::Pending;
        }
        let failure_projection = projection.clone();
        let worker_ticket = ticket.clone();
        let spawned = std::thread::Builder::new()
            .name("dsh-path-ghost-prewarm".into())
            .spawn(move || {
                let signature = super::executable_cache_signature(&paths);
                let (result, incomplete) = match loader(&paths) {
                    Ok(result) => (result, false),
                    Err(error) => {
                        tracing::warn!("PATH ghost scan failed: {error}");
                        match error.downcast_ref::<super::ExecutableScanFailure>() {
                            Some(partial) if !partial.names.is_empty() => {
                                (partial.names.clone(), true)
                            }
                            _ => {
                                fail(&projection, &worker_ticket);
                                return;
                            }
                        }
                    }
                };
                let commands = result.iter().cloned().collect();
                let mut notifier = None;
                let accepted =
                    generator::publish_system_command_scan_if(&worker_ticket, commands, || {
                        let mut state = projection.lock();
                        if state.ticket.as_ref() != Some(&worker_ticket) {
                            return false;
                        }
                        *names.write() = result.clone();
                        if incomplete {
                            state.failed();
                        } else {
                            state.phase = Phase::Ready(Instant::now());
                            state.failures = 0;
                        }
                        notifier = state.notifier.clone();
                        true
                    });
                if accepted {
                    if let Some(sender) = notifier {
                        let _ = sender.send(());
                    }
                    if !incomplete && generator::system_command_scan_is_current(&worker_ticket) {
                        let _ = super::save_cached_executables_with_signature(&result, &signature);
                    }
                } else {
                    fail(&projection, &worker_ticket);
                }
            });
        match spawned {
            Ok(worker) => Some(worker),
            Err(error) => {
                tracing::warn!("Could not start PATH ghost scan: {error}");
                fail(&failure_projection, &ticket);
                None
            }
        }
    }
}
fn fail(projection: &Mutex<PathGhost>, ticket: &SystemCommandScanTicket) {
    generator::release_system_command_scan(ticket);
    let sender = {
        let mut state = projection.lock();
        if state.ticket.as_ref() != Some(ticket) {
            return;
        }
        state.failed();
        state.notifier.clone()
    };
    if let Some(sender) = sender {
        let _ = sender.send(());
    }
}
#[cfg(test)]
mod tests;
