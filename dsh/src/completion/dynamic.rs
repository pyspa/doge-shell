use super::integrated::{CandidateType, EnhancedCandidate, matches_prefix};
use super::parser::{CompletionContext, ParsedCommandLine};
use super::shell_path::normalize_path_token;
use crate::completion::command::CompletionType;
use crate::completion::generators::filesystem::FileSystemGenerator;
use crate::environment::Environment;
use anyhow::Result;
use dsh_builtin::{project_context, task};
use parking_lot::RwLock;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing::warn;

mod cache;
mod cache_ops;
mod collectors;
mod compose;
mod container;
mod context;
mod dev;
mod engine;
mod exec_parse;
mod external;
mod git;
mod inventory;
mod kubernetes;
mod linux;
mod local;
mod platform;
mod project;
mod registry;
mod runner;
mod runtime;
mod specs;
mod system;
mod value_collectors;
mod worker;

use cache_ops::*;
use compose::*;
use context::*;
use exec_parse::*;
use inventory::*;
use system::*;

use external::pacman_sync_mode;
pub(crate) use runtime::CompletionRuntime;
use specs::CORE_LOCAL_SPECS;

use cache::{
    CommandValueCacheEntry, CommandValueErrorEntry, ComposeCacheEntry, DynamicCommandCacheKey,
    DynamicCommandCacheKind, ExternalCompletionCacheEntry, ExternalCompletionCacheKey,
    FileMetadataSignature, ProjectDynamicCache, ProjectRootCacheEntry, TaskCacheEntry,
    TaskCacheKey,
};

const DYNAMIC_COMMAND_CACHE_TTL_MS: u64 = 1000;
const DYNAMIC_COMMAND_CACHE_LIMIT: usize = 256;
const DYNAMIC_COMMAND_ERROR_BACKOFF_MS: u64 = 2000;
const REMOTE_COMMAND_CACHE_TTL: Duration = Duration::from_secs(30);
const REMOTE_COMMAND_ERROR_BACKOFF: Duration = Duration::from_secs(15);
/// Project roots do not move while the shell sits at a prompt, and finding one
/// costs a `canonicalize` plus a marker probe per ancestor directory, so this
/// cache is kept far longer than the dynamic command values cache.
const PROJECT_ROOT_CACHE_TTL_MS: u64 = 30_000;
const EXTERNAL_COMPLETION_CACHE_LIMIT: usize = 128;
const JS_PROJECT_TASK_SOURCES: &[&str] = &["npm", "pnpm", "yarn", "bun"];
const DENO_PROJECT_TASK_SOURCES: &[&str] = &["deno"];
const TURBO_PROJECT_TASK_SOURCES: &[&str] = &["turbo"];
const NX_PROJECT_TASK_SOURCES: &[&str] = &["nx"];
const MISE_PROJECT_TASK_SOURCES: &[&str] = &["mise"];
const TASKFILE_PROJECT_TASK_SOURCES: &[&str] = &["taskfile"];
const JUST_PROJECT_TASK_SOURCES: &[&str] = &["just"];
const MAKE_PROJECT_TASK_SOURCES: &[&str] = &["make"];
const GRADLE_PROJECT_TASK_SOURCES: &[&str] = &["gradle"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectTaskCandidateText {
    Name,
    NxRunArgument,
}

#[derive(Debug, Clone, Copy)]
struct ProjectTaskCompletionConfig {
    sources: &'static [&'static str],
    candidate_text: ProjectTaskCandidateText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CargoMetadataValueKind {
    Package,
    Bin,
    Example,
    Feature,
    Test,
    Bench,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemdUnitListKind {
    All,
    Running,
    Enabled,
    Disabled,
    UnitFiles,
}

/// Everything that narrows a `systemctl list-units` / `list-unit-files` query:
/// which listing to run, which manager to ask, and an optional `--type=` filter.
#[derive(Debug, Clone, Copy)]
struct SystemdUnitQuery {
    kind: SystemdUnitListKind,
    manager_scope: Option<SystemdManagerScope>,
    unit_type: Option<&'static str>,
}

impl SystemdUnitQuery {
    const fn new(
        kind: SystemdUnitListKind,
        manager_scope: Option<SystemdManagerScope>,
        unit_type: Option<&'static str>,
    ) -> Self {
        Self {
            kind,
            manager_scope,
            unit_type,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemdManagerScope {
    System,
    User,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FishFallbackMode {
    Auto,
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CachePolicy {
    CachedOnly,
    RefreshInBackground,
}

#[derive(Debug, Clone, Copy)]
struct CommandQueryPolicy {
    ttl: Duration,
    error_backoff: Duration,
}

impl CommandQueryPolicy {
    const LOCAL: Self = Self {
        ttl: Duration::from_millis(DYNAMIC_COMMAND_CACHE_TTL_MS),
        error_backoff: Duration::from_millis(DYNAMIC_COMMAND_ERROR_BACKOFF_MS),
    };
    const REMOTE: Self = Self {
        ttl: REMOTE_COMMAND_CACHE_TTL,
        error_backoff: REMOTE_COMMAND_ERROR_BACKOFF,
    };
}

impl CachePolicy {
    pub(crate) fn is_cached_only(self) -> bool {
        matches!(self, Self::CachedOnly)
    }
}

impl FishFallbackMode {
    fn label(self) -> &'static str {
        match self {
            FishFallbackMode::Auto => "auto",
            FishFallbackMode::Enabled => "enabled",
            FishFallbackMode::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct NmcliCompletionSpec<'a> {
    kind: &'a str,
    args: &'a [&'a str],
    description: &'a str,
    parser: fn(&[String]) -> Vec<String>,
}

pub(crate) struct DynamicCompletionProvider {
    environment: Arc<RwLock<Environment>>,
    cache: Arc<RwLock<ProjectDynamicCache>>,
    refresh_generation: AtomicU64,
    runtime: Arc<CompletionRuntime>,
}

#[derive(Debug, Default, Clone)]
struct DynamicCompletionDiagnostics {
    command_entries: usize,
    command_pending: usize,
    command_pruned_total: usize,
    external_entries: usize,
    external_pending: usize,
    external_fish_entries: usize,
    external_pruned_total: usize,
    last_refresh: Option<Instant>,
    last_external: Option<String>,
    provider_lines: Vec<String>,
    queue_dropped_total: u64,
}

fn diagnostics_lines(runtime: &CompletionRuntime) -> Vec<String> {
    let diagnostics = runtime.diagnostics.read().clone();
    let refresh = diagnostics
        .last_refresh
        .map(|instant| format!("{}ms-ago", instant.elapsed().as_millis()))
        .unwrap_or_else(|| "never".to_string());
    let external = diagnostics
        .last_external
        .unwrap_or_else(|| "none".to_string());

    let mut lines = vec![
        format!(
            "completion-cache dynamic-command entries={} pending={} limit={} pruned={}",
            diagnostics.command_entries,
            diagnostics.command_pending,
            DYNAMIC_COMMAND_CACHE_LIMIT,
            diagnostics.command_pruned_total
        ),
        format!(
            "completion-cache external entries={} pending={} fish={} limit={} pruned={} dropped={} timeout={}ms last={}",
            diagnostics.external_entries,
            diagnostics.external_pending,
            diagnostics.external_fish_entries,
            EXTERNAL_COMPLETION_CACHE_LIMIT,
            diagnostics.external_pruned_total,
            diagnostics.queue_dropped_total,
            runner::timeout().as_millis(),
            external
        ),
        format!("completion-cache last-refresh {refresh}"),
    ];
    lines.extend(diagnostics.provider_lines);
    lines
}

pub(crate) fn is_known_declared_dynamic_provider(provider: &str) -> bool {
    registry::registration(provider).is_some()
}

pub(crate) fn fish_fallback_mode_label(environment: &Environment) -> &'static str {
    fish_fallback_mode_from_env(environment).label()
}

fn fish_fallback_mode_from_env(environment: &Environment) -> FishFallbackMode {
    match environment
        .get_var("DOGESH_COMPLETION_FISH_FALLBACK")
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None => FishFallbackMode::Auto,
        Some(value) if env_truthy(value) => FishFallbackMode::Enabled,
        Some(value) if env_falsey(value) => FishFallbackMode::Disabled,
        Some(_) => FishFallbackMode::Disabled,
    }
}

impl DynamicCompletionProvider {
    pub(crate) fn new(environment: Arc<RwLock<Environment>>) -> Self {
        Self::with_runtime(environment, Arc::new(CompletionRuntime::new()))
    }

    pub(crate) fn with_runtime(
        environment: Arc<RwLock<Environment>>,
        runtime: Arc<CompletionRuntime>,
    ) -> Self {
        Self {
            environment,
            cache: Arc::new(RwLock::new(ProjectDynamicCache::default())),
            refresh_generation: AtomicU64::new(0),
            runtime,
        }
    }

    pub(crate) fn refresh_generation(&self) -> u64 {
        self.refresh_generation.load(Ordering::Relaxed)
    }

    pub(crate) fn has_pending_refresh(&self) -> bool {
        let cache = self.cache.read();
        !cache.command_pending.is_empty() || !cache.external_pending.is_empty()
    }

    fn mark_refresh_scheduled(&self) {
        self.refresh_generation.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn cached_project_root(&self, current_dir: &Path) -> PathBuf {
        let key = current_dir.to_path_buf();
        let ttl = Duration::from_millis(PROJECT_ROOT_CACHE_TTL_MS);
        {
            let cache = self.cache.read();
            if let Some(entry) = cache.project_roots.get(&key)
                && entry.cached_at.elapsed() < ttl
            {
                return entry.project_root.clone();
            }
        }

        let project_root = project_context::find_project_root(current_dir);
        let mut cache = self.cache.write();
        cache
            .project_roots
            .retain(|_, entry| entry.cached_at.elapsed() < ttl);
        cache.project_roots.insert(
            key,
            ProjectRootCacheEntry {
                project_root: project_root.clone(),
                cached_at: Instant::now(),
            },
        );
        project_root
    }

    pub(crate) fn collect_declared_dynamic_candidates(
        &self,
        provider: &str,
        scope: Option<&str>,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        self.try_collect_declared_dynamic_candidates(
            provider,
            scope,
            parsed_command_line,
            current_dir,
            cache_policy,
        )
        .unwrap_or_else(|| {
            warn!("Unknown dynamic completion provider: {provider}");
            Vec::new()
        })
    }

    fn try_collect_declared_dynamic_candidates(
        &self,
        provider: &str,
        scope: Option<&str>,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Option<Vec<EnhancedCandidate>> {
        let registration = registry::registration(provider)?;
        let request = registry::DynamicProviderRequest {
            provider: registration.id,
            scope,
            parsed_command_line,
            current_dir,
            cache_policy,
        };
        registration.collect(self, &request)
    }

    pub(crate) fn collect_fish_fallback_candidates(
        &self,
        current_dir: &Path,
        input: &str,
        cursor_pos: usize,
        parsed_command_line: &ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        if !self.fish_fallback_enabled() {
            return Vec::new();
        }

        let Some(command_path) = self.resolve_command_path("fish") else {
            return Vec::new();
        };

        let subcommand_path = parsed_command_line.subcommand_path.join(" ");
        let input_prefix = input_prefix_at_cursor(input, cursor_pos);
        let command_template = format!("fish-fallback:{command_path}");
        let key = ExternalCompletionCacheKey {
            command_template,
            current_dir: canonicalize_path(current_dir),
            input: input_prefix,
            cursor_pos,
            command: parsed_command_line.command.clone(),
            current_token: parsed_command_line.current_token.clone(),
            subcommand_path,
        };

        let loader_key = key.clone();
        match self.load_external_candidates(key, move || {
            run_fish_completer_for_key(&command_path, &loader_key)
        }) {
            Ok(candidates) => candidates,
            Err(err) => {
                warn!("Fish completion fallback failed: {}", err);
                Vec::new()
            }
        }
    }

    fn fish_fallback_enabled(&self) -> bool {
        fish_fallback_mode_from_env(&self.environment.read()) != FishFallbackMode::Disabled
    }

    pub(crate) fn has_async_fallback(&self) -> bool {
        let environment = self.environment.read();
        let has_external = environment
            .get_var("DOGESH_EXTERNAL_COMPLETER")
            .is_some_and(|value| !value.trim().is_empty());
        let has_fish = fish_fallback_mode_from_env(&environment) != FishFallbackMode::Disabled
            && environment.lookup("fish").is_some();
        has_external || has_fish
    }
}

#[cfg(test)]
mod tests;
