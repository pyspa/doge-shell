//! System-command completion backed exclusively by an injected logical shell PATH.
//!
//! Executable scans are cached globally for hot keypresses. A PATH activation
//! owns a generation, and every live scan additionally owns a monotonically
//! increasing scan id. Publish requires both tickets, preventing an older PATH
//! (including A -> B -> A) or an older same-generation worker from replacing a
//! newer result. Runtime requests carry their activation ticket and never
//! re-activate a stale PATH snapshot.

use crate::completion::cache::CompletionCache;
use crate::completion::command::CompletionCandidate;
use crate::completion::fuzzy_match_score;
use anyhow::Result;
use parking_lot::RwLock;
use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

const SYSTEM_COMMAND_CACHE_TTL: Duration = Duration::from_millis(2000);
const GLOBAL_SYSTEM_COMMAND_CACHE_TTL: Duration = Duration::from_secs(30);

/// Ownership of one activation of the logical shell PATH.
///
/// `paths` is captured with `generation` when a worker starts. Both fields are
/// checked at commit time: paths alone cannot distinguish A -> B -> A.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SystemCommandCacheTicket {
    generation: u64,
    paths: Arc<Vec<String>>,
}

/// Ownership of one live executable scan within a PATH generation.
///
/// The scan id orders work that shares the same generation. A scan that started
/// earlier may finish later, but it must not replace a newer scan's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SystemCommandScanTicket {
    cache: SystemCommandCacheTicket,
    scan_id: u64,
}

impl SystemCommandScanTicket {
    pub(crate) fn activation(&self) -> &SystemCommandCacheTicket {
        &self.cache
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RefreshDecision {
    Fresh,
    UseStale,
    StartBackground(SystemCommandScanTicket),
    Cold(SystemCommandScanTicket),
}

#[derive(Debug)]
struct SystemCommandCacheState {
    paths: Arc<Vec<String>>,
    commands: Option<BTreeSet<String>>,
    updated_at: Option<Instant>,
    generation: u64,
    next_scan_id: u64,
    last_published_scan_id: u64,
    live_scan_started: bool,
    inflight_scan_id: Option<u64>,
    candidate_cache: CompletionCache<CompletionCandidate>,
}

impl SystemCommandCacheState {
    fn new() -> Self {
        Self {
            paths: Arc::new(Vec::new()),
            commands: None,
            updated_at: None,
            generation: 0,
            next_scan_id: 0,
            last_published_scan_id: 0,
            live_scan_started: false,
            inflight_scan_id: None,
            candidate_cache: CompletionCache::new(SYSTEM_COMMAND_CACHE_TTL),
        }
    }

    fn ticket(&self) -> SystemCommandCacheTicket {
        SystemCommandCacheTicket {
            generation: self.generation,
            paths: Arc::clone(&self.paths),
        }
    }

    fn activate_paths(&mut self, paths: &[String]) -> SystemCommandCacheTicket {
        if self.paths.as_slice() != paths {
            self.generation = self
                .generation
                .checked_add(1)
                .expect("system command PATH generation overflow");
            self.paths = Arc::new(paths.to_vec());
            self.commands = None;
            self.updated_at = None;
            self.last_published_scan_id = 0;
            self.live_scan_started = false;
            self.inflight_scan_id = None;
            // The short-lived cache belongs to exactly one generation. Store
            // and lookup below also verify the ticket, so an older collection
            // cannot repopulate this cache after activation.
            self.candidate_cache.clear();
        }
        self.ticket()
    }

    fn is_current(&self, ticket: &SystemCommandCacheTicket) -> bool {
        self.generation == ticket.generation && self.paths.as_slice() == ticket.paths.as_slice()
    }

    fn next_scan_ticket(
        &mut self,
        activation: &SystemCommandCacheTicket,
    ) -> SystemCommandScanTicket {
        self.next_scan_id = self
            .next_scan_id
            .checked_add(1)
            .expect("system command scan id overflow");
        let scan_id = self.next_scan_id;
        if self.is_current(activation) {
            self.live_scan_started = true;
            // Any newer live scan supersedes an older background owner. The
            // older worker remains safe to publish because scan_id ordering
            // rejects it, and it cannot clear this newer owner on completion.
            self.inflight_scan_id = None;
        }
        SystemCommandScanTicket {
            cache: activation.clone(),
            scan_id,
        }
    }

    fn next_background_scan_ticket(
        &mut self,
        activation: &SystemCommandCacheTicket,
    ) -> SystemCommandScanTicket {
        let ticket = self.next_scan_ticket(activation);
        if self.is_current(&ticket.cache) {
            self.inflight_scan_id = Some(ticket.scan_id);
        }
        ticket
    }

    fn publish_live_at(
        &mut self,
        ticket: &SystemCommandScanTicket,
        commands: BTreeSet<String>,
        now: Instant,
    ) -> bool {
        if !self.is_current(&ticket.cache) || ticket.scan_id <= self.last_published_scan_id {
            return false;
        }

        self.commands = Some(commands);
        self.updated_at = Some(now);
        self.last_published_scan_id = ticket.scan_id;
        if self.inflight_scan_id == Some(ticket.scan_id) {
            self.inflight_scan_id = None;
        }
        true
    }

    fn publish_live(
        &mut self,
        ticket: &SystemCommandScanTicket,
        commands: BTreeSet<String>,
    ) -> bool {
        self.publish_live_at(ticket, commands, Instant::now())
    }

    fn publish_cached(
        &mut self,
        ticket: &SystemCommandCacheTicket,
        commands: BTreeSet<String>,
        now: Instant,
    ) -> bool {
        if !self.is_current(ticket) || self.live_scan_started {
            return false;
        }
        self.commands = Some(commands);
        self.updated_at = Some(now);
        true
    }

    fn plan_refresh_if_stale(
        &mut self,
        ticket: &SystemCommandCacheTicket,
        now: Instant,
    ) -> RefreshDecision {
        if !self.is_current(ticket) || self.commands.is_none() {
            return RefreshDecision::Cold(self.next_scan_ticket(ticket));
        }
        let stale = self.updated_at.as_ref().is_none_or(|updated_at| {
            now.saturating_duration_since(*updated_at) >= GLOBAL_SYSTEM_COMMAND_CACHE_TTL
        });
        if !stale {
            return RefreshDecision::Fresh;
        }
        if self.inflight_scan_id.is_some() {
            return RefreshDecision::UseStale;
        }

        RefreshDecision::StartBackground(self.next_background_scan_ticket(ticket))
    }

    fn lookup_candidates(
        &self,
        ticket: &SystemCommandCacheTicket,
        current_token: &str,
    ) -> Option<Vec<CompletionCandidate>> {
        if !self.is_current(ticket) {
            return None;
        }
        self.candidate_cache
            .lookup(current_token)
            .map(|hit| hit.candidates)
    }

    fn store_candidates(
        &mut self,
        ticket: &SystemCommandCacheTicket,
        current_token: &str,
        candidates: &[CompletionCandidate],
    ) -> bool {
        if !self.is_current(ticket) {
            return false;
        }
        self.candidate_cache
            .set(current_token.to_string(), candidates.to_vec());
        true
    }
}

static SYSTEM_COMMAND_CACHE: LazyLock<RwLock<SystemCommandCacheState>> =
    LazyLock::new(|| RwLock::new(SystemCommandCacheState::new()));

/// Activate `paths` and return the generation that owns all PATH-derived work.
pub(crate) fn activate_system_command_cache(paths: &[String]) -> SystemCommandCacheTicket {
    // The overwhelmingly common hot path is an unchanged PATH. Avoid taking
    // the write lock unless another activation won a race after the read.
    {
        let state = SYSTEM_COMMAND_CACHE.read();
        if state.paths.as_slice() == paths {
            return state.ticket();
        }
    }

    SYSTEM_COMMAND_CACHE.write().activate_paths(paths)
}

/// Start a live scan ordered within its PATH activation.
pub(crate) fn begin_system_command_scan(
    ticket: &SystemCommandCacheTicket,
) -> SystemCommandScanTicket {
    SYSTEM_COMMAND_CACHE.write().next_scan_ticket(ticket)
}

/// Start a live scan that owns the generation's background-refresh slot.
pub(crate) fn begin_background_system_command_scan(
    ticket: &SystemCommandCacheTicket,
) -> SystemCommandScanTicket {
    SYSTEM_COMMAND_CACHE
        .write()
        .next_background_scan_ticket(ticket)
}

/// Publish a live scan only if its activation and start order are still current.
pub(crate) fn publish_system_command_scan(
    ticket: &SystemCommandScanTicket,
    commands: BTreeSet<String>,
) -> bool {
    SYSTEM_COMMAND_CACHE.write().publish_live(ticket, commands)
}

/// Publish a persistent-cache snapshot only before any live scan has started.
pub(crate) fn publish_cached_system_commands(
    ticket: &SystemCommandCacheTicket,
    commands: BTreeSet<String>,
) -> bool {
    SYSTEM_COMMAND_CACHE
        .write()
        .publish_cached(ticket, commands, Instant::now())
}

pub(crate) fn release_system_command_scan(ticket: &SystemCommandScanTicket) {
    let mut state = SYSTEM_COMMAND_CACHE.write();
    if state.is_current(&ticket.cache) && state.inflight_scan_id == Some(ticket.scan_id) {
        state.inflight_scan_id = None;
    }
}

pub(crate) fn system_command_scan_is_current(ticket: &SystemCommandScanTicket) -> bool {
    SYSTEM_COMMAND_CACHE.read().is_current(&ticket.cache)
}

/// Update the environment-local executable-name projection under the same
/// generation check. The global-state read is intentionally held while taking
/// the projection lock: PATH activation takes these locks in the same order,
/// and projection writers must stay short and perform no filesystem I/O.
pub(crate) fn publish_environment_executable_names(
    ticket: &SystemCommandCacheTicket,
    executable_names: &RwLock<Vec<String>>,
    names: Vec<String>,
) -> bool {
    let state = SYSTEM_COMMAND_CACHE.read();
    if !state.is_current(ticket) {
        return false;
    }

    *executable_names.write() = names;
    true
}

fn lookup_candidates(
    ticket: &SystemCommandCacheTicket,
    current_token: &str,
) -> Option<Vec<CompletionCandidate>> {
    if current_token.is_empty() {
        return None;
    }
    SYSTEM_COMMAND_CACHE
        .read()
        .lookup_candidates(ticket, current_token)
}

fn store_candidate_cache(
    ticket: &SystemCommandCacheTicket,
    current_token: &str,
    candidates: &[CompletionCandidate],
) {
    if current_token.is_empty() {
        return;
    }
    SYSTEM_COMMAND_CACHE
        .write()
        .store_candidates(ticket, current_token, candidates);
}

fn plan_refresh_if_stale(ticket: &SystemCommandCacheTicket) -> RefreshDecision {
    let now = Instant::now();
    {
        let state = SYSTEM_COMMAND_CACHE.read();
        if !state.is_current(ticket) {
            drop(state);
            return RefreshDecision::Cold(begin_system_command_scan(ticket));
        }
        if state
            .commands
            .as_ref()
            .zip(state.updated_at.as_ref())
            .is_some_and(|(_, updated_at)| {
                now.saturating_duration_since(*updated_at) < GLOBAL_SYSTEM_COMMAND_CACHE_TTL
            })
        {
            return RefreshDecision::Fresh;
        }
    }

    SYSTEM_COMMAND_CACHE
        .write()
        .plan_refresh_if_stale(ticket, now)
}

fn spawn_refresh(ticket: SystemCommandScanTicket) {
    let worker_ticket = ticket.clone();
    let spawn_result = std::thread::Builder::new()
        .name("dsh-system-command-refresh".to_string())
        .spawn(move || {
            let commands =
                crate::environment::collect_executables(&worker_ticket.activation().paths)
                    .into_iter()
                    .collect();
            publish_system_command_scan(&worker_ticket, commands);
        });

    if let Err(error) = spawn_result {
        tracing::warn!("Failed to start system command cache refresh: {error}");
        release_system_command_scan(&ticket);
    }
}

fn append_command_candidates(
    commands: &BTreeSet<String>,
    current_token: &str,
    candidates: &mut Vec<CompletionCandidate>,
    seen_names: &mut HashSet<String>,
) {
    for command in commands
        .iter()
        .filter(|command| fuzzy_match_score(command, current_token).is_some())
    {
        if candidates.len() >= crate::completion::MAX_RESULT {
            break;
        }
        if seen_names.insert(command.to_string()) {
            candidates.push(CompletionCandidate::subcommand(command.to_string(), None));
        }
    }
}

fn append_cached_command_candidates(
    ticket: &SystemCommandCacheTicket,
    current_token: &str,
    candidates: &mut Vec<CompletionCandidate>,
    seen_names: &mut HashSet<String>,
) {
    let state = SYSTEM_COMMAND_CACHE.read();
    if !state.is_current(ticket) {
        return;
    }
    let Some(commands) = state.commands.as_ref() else {
        return;
    };

    append_command_candidates(commands, current_token, candidates, seen_names);
}

/// Generates candidates from one explicit logical shell PATH activation.
pub struct SystemCommandGenerator {
    ticket: SystemCommandCacheTicket,
}

impl SystemCommandGenerator {
    pub fn new(paths: &[String]) -> Self {
        Self::from_activation(activate_system_command_cache(paths))
    }

    pub(crate) fn from_activation(ticket: SystemCommandCacheTicket) -> Self {
        Self { ticket }
    }

    pub fn generate_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
        let ticket = &self.ticket;
        if let Some(hit) = lookup_candidates(ticket, current_token) {
            return Ok(hit);
        }

        let mut candidates = Vec::with_capacity(32);
        let mut seen_names: HashSet<String> = HashSet::new();

        // Absolute and relative explicit paths do not depend on PATH and keep
        // their existing direct-file behavior.
        if (current_token.starts_with('/') || current_token.starts_with("./"))
            && Path::new(current_token).is_file()
        {
            candidates.push(CompletionCandidate::subcommand(
                current_token.to_string(),
                None,
            ));
            seen_names.insert(current_token.to_string());
        }

        match plan_refresh_if_stale(ticket) {
            RefreshDecision::Fresh | RefreshDecision::UseStale => {
                append_cached_command_candidates(
                    ticket,
                    current_token,
                    &mut candidates,
                    &mut seen_names,
                );
            }
            RefreshDecision::StartBackground(scan_ticket) => {
                // Stale-while-revalidate: use the current generation's cache
                // immediately and scan the captured logical PATH off-thread.
                spawn_refresh(scan_ticket);
                append_cached_command_candidates(
                    ticket,
                    current_token,
                    &mut candidates,
                    &mut seen_names,
                );
            }
            RefreshDecision::Cold(scan_ticket) => {
                // Only a cold cache scans on the keypress path. The cache write
                // lock and Environment lock are both absent during this I/O.
                let commands: BTreeSet<String> =
                    crate::environment::collect_executables(&ticket.paths)
                        .into_iter()
                        .collect();
                let _ = publish_system_command_scan(&scan_ticket, commands.clone());
                append_command_candidates(
                    &commands,
                    current_token,
                    &mut candidates,
                    &mut seen_names,
                );
            }
        }

        store_candidate_cache(ticket, current_token, &candidates);
        Ok(candidates)
    }
}

#[cfg(test)]
mod tests;
