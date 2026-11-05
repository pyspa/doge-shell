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
use std::path::{Path, PathBuf};
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
    scan_paths: Option<Arc<Vec<PathBuf>>>,
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
    pub(crate) fn order_key(&self) -> (u64, u64) {
        (self.cache.generation, self.scan_id)
    }
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
    scan_paths: Option<Arc<Vec<PathBuf>>>,
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
            scan_paths: None,
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
            scan_paths: self.scan_paths.clone(),
        }
    }

    fn activate_paths(&mut self, paths: &[String]) -> SystemCommandCacheTicket {
        self.activate_paths_at(paths, None)
    }

    fn activate_paths_at(
        &mut self,
        paths: &[String],
        scan_paths: Option<Arc<Vec<PathBuf>>>,
    ) -> SystemCommandCacheTicket {
        if self.paths.as_slice() != paths || self.scan_paths != scan_paths {
            self.generation = self
                .generation
                .checked_add(1)
                .expect("system command PATH generation overflow");
            self.paths = Arc::new(paths.to_vec());
            self.scan_paths = scan_paths;
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
        self.generation == ticket.generation
            && self.paths.as_slice() == ticket.paths.as_slice()
            && self.scan_paths == ticket.scan_paths
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

        self.candidate_cache.clear();
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
        self.candidate_cache.clear();
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

    fn plan_background(
        &mut self,
        ticket: &SystemCommandCacheTicket,
    ) -> Option<SystemCommandScanTicket> {
        if !self.is_current(ticket) || self.inflight_scan_id.is_some() {
            return None;
        }
        if self.commands.is_some()
            && self
                .updated_at
                .is_some_and(|at| at.elapsed() < GLOBAL_SYSTEM_COMMAND_CACHE_TTL)
        {
            return None;
        }
        Some(self.next_background_scan_ticket(ticket))
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
        if state.paths.as_slice() == paths && state.scan_paths.is_none() {
            return state.ticket();
        }
    }

    SYSTEM_COMMAND_CACHE.write().activate_paths(paths)
}

/// Publication identity for upper-level interactive caches. Captured before
/// collection, so a concurrent publication cannot give partial results a new key.
pub(crate) fn system_command_cache_revision() -> (u64, u64) {
    let state = SYSTEM_COMMAND_CACHE.read();
    (state.generation, state.last_published_scan_id)
}

/// Anchor relative and empty entries at the request boundary, before any worker
/// starts. The resolved paths also distinguish caches for different cwd values.
pub(crate) fn activate_system_command_cache_at(
    paths: &[String],
    filesystem: &super::filesystem::FileSystemContext,
) -> SystemCommandCacheTicket {
    if paths.iter().all(|path| Path::new(path).is_absolute()) {
        return activate_system_command_cache(paths);
    }
    let scan_paths = Some(Arc::new(
        paths
            .iter()
            .map(|path| filesystem.lookup_directory(path, false))
            .collect(),
    ));
    {
        let state = SYSTEM_COMMAND_CACHE.read();
        if state.paths.as_slice() == paths && state.scan_paths == scan_paths {
            return state.ticket();
        }
    }
    SYSTEM_COMMAND_CACHE
        .write()
        .activate_paths_at(paths, scan_paths)
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

/// Claim the existing worker slot without superseding an active worker.
pub(crate) fn claim_background_system_command_scan(
    ticket: &SystemCommandCacheTicket,
) -> Option<SystemCommandScanTicket> {
    let mut state = SYSTEM_COMMAND_CACHE.write();
    if !state.is_current(ticket) || state.inflight_scan_id.is_some() {
        return None;
    }
    Some(state.next_background_scan_ticket(ticket))
}

/// Publish a live scan only if its activation and start order are still current.
pub(crate) fn publish_system_command_scan(
    ticket: &SystemCommandScanTicket,
    commands: BTreeSet<String>,
) -> bool {
    SYSTEM_COMMAND_CACHE.write().publish_live(ticket, commands)
}

/// Publish the local projection and global result under the same ownership check.
/// The callback must be short, contain no I/O and follow state -> projection lock order.
pub(crate) fn publish_system_command_scan_if(
    ticket: &SystemCommandScanTicket,
    commands: BTreeSet<String>,
    publish_projection: impl FnOnce() -> bool,
) -> bool {
    let mut state = SYSTEM_COMMAND_CACHE.write();
    if !state.is_current(&ticket.cache)
        || ticket.scan_id <= state.last_published_scan_id
        || !publish_projection()
    {
        return false;
    }
    state.publish_live(ticket, commands)
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

fn collect_ticket_commands(ticket: &SystemCommandCacheTicket) -> Vec<String> {
    ticket.scan_paths.as_ref().map_or_else(
        || crate::environment::collect_executables(&ticket.paths),
        |paths| crate::environment::collect_executables_from_paths(paths),
    )
}

fn spawn_refresh(ticket: SystemCommandScanTicket) {
    let worker_ticket = ticket.clone();
    let spawn_result = std::thread::Builder::new()
        .name("dsh-system-command-refresh".to_string())
        .spawn(move || {
            let commands = collect_ticket_commands(worker_ticket.activation())
                .into_iter()
                .collect();
            publish_system_command_scan(&worker_ticket, commands);
        });

    if let Err(error) = spawn_result {
        tracing::warn!("Failed to start system command cache refresh: {error}");
        release_system_command_scan(&ticket);
    }
}

fn append_explicit_command(
    current_token: &str,
    candidates: &mut Vec<CompletionCandidate>,
    seen_names: &mut HashSet<String>,
) {
    if (current_token.starts_with('/') || current_token.starts_with("./"))
        && Path::new(current_token).is_file()
    {
        candidates.push(CompletionCandidate::subcommand(
            current_token.to_string(),
            None,
        ));
        seen_names.insert(current_token.to_string());
    }
}

fn append_command_candidates(
    commands: &BTreeSet<String>,
    current_token: &str,
    candidates: &mut Vec<CompletionCandidate>,
    seen_names: &mut HashSet<String>,
    limit: usize,
) {
    for command in commands
        .iter()
        .filter(|command| fuzzy_match_score(command, current_token).is_some())
    {
        if candidates.len() >= limit {
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
    limit: usize,
) {
    let state = SYSTEM_COMMAND_CACHE.read();
    if !state.is_current(ticket) {
        return;
    }
    let Some(commands) = state.commands.as_ref() else {
        return;
    };

    append_command_candidates(commands, current_token, candidates, seen_names, limit);
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

    /// Interactive requests borrow ready data and share the existing prewarm /
    /// refresh slot. No filesystem enumeration is awaited on the input path.
    pub(crate) fn generate_in_background(
        &self,
        current_token: &str,
        runtime: Arc<crate::completion::dynamic::CompletionRuntime>,
        limit: usize,
    ) -> Vec<CompletionCandidate> {
        self.generate_in_background_with(
            current_token,
            runtime,
            limit,
            crate::environment::collect_executables_from_paths,
        )
    }

    fn generate_in_background_with<L>(
        &self,
        current_token: &str,
        runtime: Arc<crate::completion::dynamic::CompletionRuntime>,
        limit: usize,
        loader: L,
    ) -> Vec<CompletionCandidate>
    where
        L: FnOnce(&[PathBuf]) -> Vec<String> + Send + 'static,
    {
        let scan = SYSTEM_COMMAND_CACHE.write().plan_background(&self.ticket);
        if let Some(ticket) = scan {
            let worker_ticket = ticket.clone();
            let spawn = std::thread::Builder::new()
                .name("dsh-system-command-refresh".into())
                .spawn(move || {
                    let paths = worker_ticket.cache.scan_paths.clone().unwrap_or_else(|| {
                        Arc::new(
                            worker_ticket
                                .cache
                                .paths
                                .iter()
                                .map(PathBuf::from)
                                .collect(),
                        )
                    });
                    let commands = loader(&paths).into_iter().collect();
                    if publish_system_command_scan(&worker_ticket, commands) {
                        runtime.notify();
                    }
                });
            if let Err(error) = spawn {
                tracing::warn!("Failed to start system command cache refresh: {error}");
                release_system_command_scan(&ticket);
            }
        }
        let mut candidates = Vec::new();
        let mut seen_names = HashSet::new();
        append_explicit_command(current_token, &mut candidates, &mut seen_names);
        append_cached_command_candidates(
            &self.ticket,
            current_token,
            &mut candidates,
            &mut seen_names,
            limit,
        );
        candidates
    }

    pub(crate) fn generate_for_runtime(
        &self,
        current_token: &str,
        runtime: Option<&Arc<crate::completion::dynamic::CompletionRuntime>>,
    ) -> Result<Vec<CompletionCandidate>> {
        match runtime {
            Some(runtime) => Ok(self.generate_in_background(
                current_token,
                runtime.clone(),
                crate::completion::MAX_RESULT,
            )),
            None => self.generate_candidates(current_token),
        }
    }

    pub fn generate_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
        let ticket = &self.ticket;
        if let Some(hit) = lookup_candidates(ticket, current_token) {
            return Ok(hit);
        }

        let mut candidates = Vec::with_capacity(32);
        let mut seen_names: HashSet<String> = HashSet::new();

        // Explicit filenames retain their direct-file behavior in both modes.
        append_explicit_command(current_token, &mut candidates, &mut seen_names);

        match plan_refresh_if_stale(ticket) {
            RefreshDecision::Fresh | RefreshDecision::UseStale => {
                append_cached_command_candidates(
                    ticket,
                    current_token,
                    &mut candidates,
                    &mut seen_names,
                    crate::completion::MAX_RESULT,
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
                    crate::completion::MAX_RESULT,
                );
            }
            RefreshDecision::Cold(scan_ticket) => {
                // Only a cold cache scans on the keypress path. The cache write
                // lock and Environment lock are both absent during this I/O.
                let commands: BTreeSet<String> =
                    collect_ticket_commands(ticket).into_iter().collect();
                let _ = publish_system_command_scan(&scan_ticket, commands.clone());
                append_command_candidates(
                    &commands,
                    current_token,
                    &mut candidates,
                    &mut seen_names,
                    crate::completion::MAX_RESULT,
                );
            }
        }

        store_candidate_cache(ticket, current_token, &candidates);
        Ok(candidates)
    }
}

#[cfg(test)]
mod tests;
