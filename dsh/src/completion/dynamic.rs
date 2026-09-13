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
mod compose;
mod container;
mod context;
mod dev;
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
        .get_var("DSH_COMPLETION_FISH_FALLBACK")
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
            .get_var("DSH_EXTERNAL_COMPLETER")
            .is_some_and(|value| !value.trim().is_empty());
        let has_fish = fish_fallback_mode_from_env(&environment) != FishFallbackMode::Disabled
            && environment.lookup("fish").is_some();
        has_external || has_fish
    }

    pub(crate) fn collect_journalctl_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let CompletionContext::OptionValue { option_name, .. } =
            &parsed_command_line.completion_context
        else {
            return Vec::new();
        };
        if !matches!(option_name.as_str(), "-u" | "--unit") {
            return Vec::new();
        }

        self.collect_systemd_unit_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            SystemdUnitQuery::new(
                SystemdUnitListKind::All,
                selected_systemd_manager_scope(parsed_command_line),
                None,
            ),
            "systemd unit",
            cached_only,
        )
    }

    pub(crate) fn collect_tmux_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let completes_session = match &parsed_command_line.completion_context {
            CompletionContext::OptionValue { option_name, .. } => option_name == "-t",
            CompletionContext::SubCommand | CompletionContext::Argument { .. } => matches!(
                parsed_command_line
                    .subcommand_path
                    .first()
                    .map(String::as_str),
                Some("attach-session" | "attach" | "a" | "kill-session")
            ),
            _ => false,
        };
        if !completes_session {
            return Vec::new();
        }

        local::collect_by_id(
            self,
            "tmux.session",
            parsed_command_line,
            current_dir,
            cached_only,
        )
    }

    pub(crate) fn collect_screen_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::OptionValue { .. }
                | CompletionContext::SubCommand
                | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }

        local::collect_by_id(
            self,
            "screen.session",
            parsed_command_line,
            current_dir,
            cached_only,
        )
    }

    pub(crate) fn collect_process_name_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        command_name: &str,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }
        self.collect_cached_value_candidates(
            command_name,
            "process-name",
            PathBuf::from("/proc"),
            parsed_command_line.current_token.as_str(),
            "process name",
            cached_only,
            || Ok(load_process_names()),
        )
    }

    fn collect_process_pid_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::OptionValue { .. }
                | CompletionContext::SubCommand
                | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }
        self.collect_cached_value_candidates(
            "system",
            "process-pid",
            PathBuf::from("/proc"),
            parsed_command_line.current_token.as_str(),
            "process id",
            cached_only,
            || Ok(load_process_ids()),
        )
    }

    pub(crate) fn collect_pip_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        command_name: &str,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line
                .subcommand_path
                .first()
                .map(String::as_str),
            Some("show" | "uninstall")
        ) || !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }

        self.collect_pip_installed_package_candidates(
            current_dir,
            command_name,
            parsed_command_line.current_token.as_str(),
            cached_only,
        )
    }

    pub(crate) fn collect_rustup_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let path = parsed_command_line
            .subcommand_path
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let completes_toolchain =
            matches!(path.as_slice(), ["default"] | ["toolchain", "uninstall"]);
        if !completes_toolchain {
            return Vec::new();
        }
        local::collect_by_id(
            self,
            "rustup.toolchain",
            parsed_command_line,
            current_dir,
            cached_only,
        )
    }

    pub(crate) fn collect_gh_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let path = parsed_command_line
            .subcommand_path
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let (value_kind, args, description) = match path.as_slice() {
            [
                "pr",
                "view" | "checkout" | "close" | "merge" | "ready" | "diff" | "comment",
            ] => (
                "pr-number",
                vec!["pr", "list", "--json", "number", "--jq", ".[].number"],
                "GitHub pull request",
            ),
            ["issue", "view" | "close" | "reopen" | "comment"] => (
                "issue-number",
                vec!["issue", "list", "--json", "number", "--jq", ".[].number"],
                "GitHub issue",
            ),
            ["run", "view" | "watch" | "download" | "rerun" | "cancel"] => (
                "run-id",
                vec![
                    "run",
                    "list",
                    "--json",
                    "databaseId",
                    "--jq",
                    ".[].databaseId",
                ],
                "GitHub Actions run",
            ),
            _ => return Vec::new(),
        };
        let command_path = self.resolve_command_path("gh");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "gh",
            value_kind,
            self.cached_project_root(&current_dir),
            parsed_command_line.current_token.as_str(),
            description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                run_command_lines(&command_path, &args, &current_dir)
            },
        )
    }

    pub(crate) fn collect_nmcli_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let path = parsed_command_line
            .subcommand_path
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let (kind, args, description) = match path.as_slice() {
            ["connection", "up" | "modify" | "delete"] => (
                "connection",
                vec!["-t", "-f", "NAME", "connection", "show"],
                "NetworkManager connection",
            ),
            ["connection", "down"] => (
                "active-connection",
                vec!["-t", "-f", "NAME", "connection", "show", "--active"],
                "active NetworkManager connection",
            ),
            ["device", "show" | "connect"] => (
                "device",
                vec!["-t", "-f", "DEVICE", "device"],
                "NetworkManager device",
            ),
            ["device", "disconnect"] => (
                "connected-device",
                vec!["-t", "-f", "DEVICE,STATE", "device", "status"],
                "connected NetworkManager device",
            ),
            _ => return Vec::new(),
        };
        let parser = if kind == "connected-device" {
            parse_nmcli_connected_devices
        } else {
            parse_nmcli_first_field
        };
        let spec = NmcliCompletionSpec {
            kind,
            args: &args,
            description,
            parser,
        };
        self.collect_nmcli_value_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            spec,
            cached_only,
        )
    }

    pub(crate) fn collect_mount_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }

        let mut candidates = local::collect_by_id(
            self,
            "block.device",
            parsed_command_line,
            current_dir,
            cached_only,
        );
        candidates.extend(local::collect_by_id(
            self,
            "fstab.mountpoint",
            parsed_command_line,
            current_dir,
            cached_only,
        ));
        candidates
    }

    pub(crate) fn collect_umount_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }
        self.collect_mountpoint_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            cached_only,
        )
    }

    pub(crate) fn collect_modprobe_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }
        // `modprobe -r` unloads, so only modules already in the kernel apply.
        let scope = parsed_command_line
            .raw_args
            .iter()
            .any(|arg| matches!(arg.as_str(), "-r" | "--remove"))
            .then_some("loaded");
        self.collect_kernel_module_candidates(
            scope,
            parsed_command_line.current_token.as_str(),
            cached_only,
        )
    }

    fn collect_nmcli_value_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        spec: NmcliCompletionSpec<'_>,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("nmcli");
        let current_dir = current_dir.to_path_buf();
        let args = spec
            .args
            .iter()
            .map(|arg| arg.to_string())
            .collect::<Vec<_>>();
        self.collect_cached_value_candidates(
            "nmcli",
            spec.kind,
            canonicalize_path(&current_dir),
            current_token,
            spec.description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let args = args.iter().map(String::as_str).collect::<Vec<_>>();
                let lines = run_command_lines(&command_path, &args, &current_dir)?;
                Ok((spec.parser)(&lines))
            },
        )
    }

    fn collect_pip_installed_package_candidates(
        &self,
        current_dir: &Path,
        command_name: &str,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path(command_name);
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            command_name,
            "installed-package",
            canonicalize_path(&current_dir),
            current_token,
            "installed python package",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_pip_freeze_packages(&run_command_lines(
                    &command_path,
                    &["list", "--format=freeze"],
                    &current_dir,
                )?))
            },
        )
    }

    fn collect_mountpoint_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("findmnt");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "umount",
            "mount-target",
            canonicalize_path(&current_dir),
            current_token,
            "mount target",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(
                    run_command_lines(&command_path, &["-rno", "TARGET"], &current_dir)?
                        .into_iter()
                        .filter(|target| target != "/")
                        .collect(),
                )
            },
        )
    }

    /// `scope: "loaded"` restricts the candidates to the modules currently in
    /// the kernel, which is what `rmmod` and friends can actually act on. The
    /// default lists every installable module, as `modprobe` needs.
    fn collect_kernel_module_candidates(
        &self,
        scope: Option<&str>,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if scope == Some("loaded") {
            return self.collect_cached_value_candidates(
                "lsmod",
                "loaded-kernel-module",
                PathBuf::from("/proc/modules"),
                current_token,
                "loaded kernel module",
                cached_only,
                || Ok(load_loaded_kernel_module_names(Path::new("/proc/modules"))),
            );
        }
        self.collect_cached_value_candidates(
            "modprobe",
            "kernel-module",
            PathBuf::from("/lib/modules"),
            current_token,
            "kernel module",
            cached_only,
            || Ok(load_kernel_module_names()),
        )
    }

    fn collect_blkid_attribute_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        attribute: &'static str,
        description: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("blkid");
        let current_dir = current_dir.to_path_buf();
        let value_kind = attribute.to_ascii_lowercase();
        self.collect_cached_value_candidates(
            "blkid",
            &value_kind,
            PathBuf::from("/run/blkid"),
            current_token,
            description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_blkid_export_attribute(
                    &run_command_lines(&command_path, &["-o", "export"], &current_dir)?,
                    attribute,
                ))
            },
        )
    }

    fn collect_sysctl_key_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if current_token.contains('=') {
            return Vec::new();
        }
        self.collect_cached_value_candidates(
            "sysctl",
            "key",
            PathBuf::from("/proc/sys"),
            current_token,
            "sysctl key",
            cached_only,
            || Ok(load_sysctl_keys()),
        )
    }

    fn collect_wireguard_config_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        collect_wireguard_config_names_from_dirs([Path::new("/etc/wireguard"), current_dir])
            .into_iter()
            .filter(|value| matches_prefix(current_token, value))
            .map(|value| EnhancedCandidate {
                text: value,
                description: Some("WireGuard config".to_string()),
                candidate_type: CandidateType::Argument,
                priority: 130,
            })
            .collect()
    }

    pub(crate) fn collect_tcpdump_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let CompletionContext::OptionValue { option_name, .. } =
            &parsed_command_line.completion_context
        else {
            return Vec::new();
        };
        if option_name != "-i" {
            return Vec::new();
        }
        self.collect_cached_value_candidates(
            "tcpdump",
            "interface",
            PathBuf::from("/sys/class/net"),
            parsed_command_line.current_token.as_str(),
            "network interface",
            cached_only,
            || Ok(load_network_interfaces()),
        )
    }

    fn collect_cargo_feature_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let (completed_prefix, active_token) = cargo_feature_token_parts(current_token);
        let mut candidates = self.collect_cargo_metadata_candidates(
            current_dir,
            active_token,
            CargoMetadataValueKind::Feature,
            "cargo feature",
            cached_only,
        );
        if !completed_prefix.is_empty() {
            for candidate in &mut candidates {
                candidate.text.insert_str(0, completed_prefix);
            }
        }
        candidates
    }

    fn collect_owner_group_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let values = self.load_or_lookup_command_values(
            "system",
            "owner-group",
            PathBuf::from("/etc"),
            cached_only,
            CommandQueryPolicy::LOCAL,
            || Ok(load_owner_group_values()),
        );
        owner_group_candidates(&values, current_token)
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_cached_value_candidates<F>(
        &self,
        command_name: &str,
        value_kind: &str,
        scope_dir: PathBuf,
        current_token: &str,
        description: &str,
        cached_only: bool,
        loader: F,
    ) -> Vec<EnhancedCandidate>
    where
        F: FnOnce() -> Result<Vec<String>> + Send + 'static,
    {
        self.collect_cached_value_candidates_with_policy(
            command_name,
            value_kind,
            scope_dir,
            current_token,
            description,
            cached_only,
            CommandQueryPolicy::LOCAL,
            loader,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_cached_value_candidates_with_policy<F>(
        &self,
        command_name: &str,
        value_kind: &str,
        scope_dir: PathBuf,
        current_token: &str,
        description: &str,
        cached_only: bool,
        query_policy: CommandQueryPolicy,
        loader: F,
    ) -> Vec<EnhancedCandidate>
    where
        F: FnOnce() -> Result<Vec<String>> + Send + 'static,
    {
        let values = self.load_or_lookup_command_values(
            command_name,
            value_kind,
            scope_dir,
            cached_only,
            query_policy,
            loader,
        );

        cached_value_matches(values, current_token)
            .into_iter()
            .map(|value| EnhancedCandidate {
                text: value,
                description: Some(description.to_string()),
                candidate_type: CandidateType::Argument,
                priority: 130,
            })
            .collect()
    }

    fn load_or_lookup_command_values<F>(
        &self,
        command_name: &str,
        value_kind: &str,
        scope_dir: PathBuf,
        cached_only: bool,
        query_policy: CommandQueryPolicy,
        loader: F,
    ) -> Vec<String>
    where
        F: FnOnce() -> Result<Vec<String>> + Send + 'static,
    {
        let kind = DynamicCommandCacheKind::CommandValue {
            command: command_name.to_string(),
            value_kind: value_kind.to_string(),
        };
        if cached_only {
            self.lookup_command_values(kind, scope_dir)
        } else {
            self.load_command_values_with_policy(kind, scope_dir, query_policy, loader)
        }
    }

    fn collect_cargo_metadata_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        kind: CargoMetadataValueKind,
        description: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("cargo");
        let current_dir = current_dir.to_path_buf();
        let scope_dir = self.cached_project_root(&current_dir);
        let value_kind = match kind {
            CargoMetadataValueKind::Package => "package",
            CargoMetadataValueKind::Bin => "bin",
            CargoMetadataValueKind::Example => "example",
            CargoMetadataValueKind::Feature => "feature",
            CargoMetadataValueKind::Test => "test",
            CargoMetadataValueKind::Bench => "bench",
        };
        self.collect_cached_value_candidates(
            "cargo",
            value_kind,
            scope_dir,
            current_token,
            description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let output = run_command_stdout(
                    &command_path,
                    &["metadata", "--no-deps", "--format-version", "1"],
                    &current_dir,
                )?;
                Ok(parse_cargo_metadata_values(&output, kind))
            },
        )
    }

    fn collect_systemd_unit_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        query: SystemdUnitQuery,
        description: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let SystemdUnitQuery {
            kind,
            manager_scope,
            unit_type,
        } = query;
        let command_path = self.resolve_command_path("systemctl");
        let current_dir = current_dir.to_path_buf();
        let base_value_kind = match kind {
            SystemdUnitListKind::All => "unit-all",
            SystemdUnitListKind::Running => "unit-running",
            SystemdUnitListKind::Enabled => "unit-enabled",
            SystemdUnitListKind::Disabled => "unit-disabled",
            SystemdUnitListKind::UnitFiles => "unit-files",
        };
        let value_kind = match manager_scope {
            Some(SystemdManagerScope::System) => format!("system-{base_value_kind}"),
            Some(SystemdManagerScope::User) => format!("user-{base_value_kind}"),
            Some(SystemdManagerScope::Global) => format!("global-{base_value_kind}"),
            None => base_value_kind.to_string(),
        };
        let value_kind = match unit_type {
            Some(unit_type) => format!("{value_kind}:{unit_type}"),
            None => value_kind,
        };
        self.collect_cached_value_candidates(
            "systemctl",
            &value_kind,
            canonicalize_path(&current_dir),
            current_token,
            description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let mut args: Vec<&str> = Vec::new();
                match manager_scope {
                    Some(SystemdManagerScope::System) => args.push("--system"),
                    Some(SystemdManagerScope::User) => args.push("--user"),
                    Some(SystemdManagerScope::Global) => args.push("--global"),
                    None => {}
                }
                args.extend(match kind {
                    SystemdUnitListKind::All => {
                        vec!["list-units", "--all", "--no-pager", "--no-legend"]
                    }
                    SystemdUnitListKind::Running => {
                        vec!["list-units", "--state=running", "--no-pager", "--no-legend"]
                    }
                    SystemdUnitListKind::Enabled => vec![
                        "list-unit-files",
                        "--state=enabled",
                        "--no-pager",
                        "--no-legend",
                    ],
                    SystemdUnitListKind::Disabled => vec![
                        "list-unit-files",
                        "--state=disabled",
                        "--no-pager",
                        "--no-legend",
                    ],
                    SystemdUnitListKind::UnitFiles => {
                        vec!["list-unit-files", "--no-pager", "--no-legend"]
                    }
                });
                if let Some(unit_type) = unit_type {
                    args.push(unit_type);
                }
                Ok(parse_first_fields(&run_command_lines(
                    &command_path,
                    &args,
                    &current_dir,
                )?))
            },
        )
    }

    pub(crate) fn collect_js_dependency_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        command_name: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }

        let project_root = self.cached_project_root(current_dir);
        let package_json = project_root.join("package.json");
        self.collect_cached_value_candidates(
            command_name,
            "package-json-dependency",
            project_root,
            parsed_command_line.current_token.as_str(),
            "package.json dependency",
            cached_only,
            move || Ok(load_package_json_dependencies(&package_json)),
        )
    }

    fn collect_cached_command_candidates<F>(
        &self,
        kind: DynamicCommandCacheKind,
        scope_dir: PathBuf,
        current_token: &str,
        description: &str,
        cached_only: bool,
        loader: F,
    ) -> Vec<EnhancedCandidate>
    where
        F: FnOnce() -> Result<Vec<String>> + Send + 'static,
    {
        let values = if cached_only {
            self.lookup_command_values(kind, scope_dir)
        } else {
            self.load_command_values(kind, scope_dir, loader)
        };

        cached_value_matches(values, current_token)
            .into_iter()
            .map(|value| EnhancedCandidate {
                text: value,
                description: Some(description.to_string()),
                candidate_type: CandidateType::Argument,
                priority: 130,
            })
            .collect()
    }

    pub(crate) fn collect_probe_cached_command_candidates(
        &self,
        scope_dir: PathBuf,
        current_token: &str,
        values: Vec<String>,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitBranch,
            scope_dir,
            current_token,
            "latency probe",
            false,
            move || Ok(values),
        )
    }

    fn load_project_tasks(&self, current_dir: &Path) -> Result<Vec<task::TaskInfo>> {
        let project_root = self.cached_project_root(current_dir);
        let cache_key = TaskCacheKey {
            project_root: project_root.clone(),
            sources: Vec::new(),
        };
        let signature = task_completion_signature(&project_root, None);

        if let Some(tasks) = self.lookup_task_cache(&cache_key, &signature) {
            return Ok(tasks);
        }

        let tasks = task::list_tasks_in_dir(&project_root)?;
        self.cache.write().tasks.insert(
            cache_key,
            TaskCacheEntry {
                signature,
                tasks: tasks.clone(),
            },
        );
        Ok(tasks)
    }

    fn lookup_project_tasks(&self, current_dir: &Path) -> Vec<task::TaskInfo> {
        let project_root = self.cached_project_root(current_dir);
        let cache_key = TaskCacheKey {
            project_root: project_root.clone(),
            sources: Vec::new(),
        };
        let signature = task_completion_signature(&project_root, None);
        self.lookup_task_cache(&cache_key, &signature)
            .unwrap_or_default()
    }

    fn load_project_tasks_for_sources(
        &self,
        current_dir: &Path,
        sources: &[&str],
    ) -> Result<Vec<task::TaskInfo>> {
        let project_root = self.cached_project_root(current_dir);
        let cache_key = TaskCacheKey {
            project_root: project_root.clone(),
            sources: normalized_task_sources(sources),
        };
        let signature = task_completion_signature(&project_root, Some(sources));

        if let Some(tasks) = self.lookup_task_cache(&cache_key, &signature) {
            return Ok(tasks);
        }

        let tasks = task::list_tasks_in_dir_for_sources(&project_root, sources)?;
        self.cache.write().tasks.insert(
            cache_key,
            TaskCacheEntry {
                signature,
                tasks: tasks.clone(),
            },
        );
        Ok(tasks)
    }

    fn lookup_project_tasks_for_sources(
        &self,
        current_dir: &Path,
        sources: &[&str],
    ) -> Vec<task::TaskInfo> {
        let project_root = self.cached_project_root(current_dir);
        let cache_key = TaskCacheKey {
            project_root: project_root.clone(),
            sources: normalized_task_sources(sources),
        };
        let signature = task_completion_signature(&project_root, Some(sources));
        self.lookup_task_cache(&cache_key, &signature)
            .unwrap_or_default()
    }

    fn lookup_task_cache(
        &self,
        cache_key: &TaskCacheKey,
        signature: &[FileMetadataSignature],
    ) -> Option<Vec<task::TaskInfo>> {
        let cache = self.cache.read();
        let entry = cache.tasks.get(cache_key)?;
        if entry.signature == signature {
            Some(entry.tasks.clone())
        } else {
            None
        }
    }

    fn load_compose_services(
        &self,
        current_dir: &Path,
        compose_file_override: Option<&Path>,
    ) -> Result<Option<(PathBuf, Vec<String>)>> {
        let compose_file = if let Some(path) = compose_file_override {
            path.to_path_buf()
        } else {
            let Some(compose_file) = find_compose_file(current_dir) else {
                return Ok(None);
            };
            compose_file
        };
        let cache_key = canonicalize_path(&compose_file);
        let signature = file_metadata_signature(&cache_key);

        if let Some(services) = self.lookup_compose_cache(&cache_key, &signature) {
            return Ok(Some((cache_key, services)));
        }

        let services = parse_compose_service_names(&cache_key)?;
        self.cache.write().compose_services.insert(
            cache_key.clone(),
            ComposeCacheEntry {
                signature,
                services: services.clone(),
            },
        );

        Ok(Some((cache_key, services)))
    }

    fn lookup_compose_cache(
        &self,
        compose_file: &Path,
        signature: &FileMetadataSignature,
    ) -> Option<Vec<String>> {
        let cache = self.cache.read();
        let entry = cache.compose_services.get(compose_file)?;
        if entry.signature == *signature {
            Some(entry.services.clone())
        } else {
            None
        }
    }

    fn load_command_values<F>(
        &self,
        kind: DynamicCommandCacheKind,
        scope_dir: PathBuf,
        loader: F,
    ) -> Vec<String>
    where
        F: FnOnce() -> Result<Vec<String>> + Send + 'static,
    {
        self.load_command_values_with_policy(kind, scope_dir, CommandQueryPolicy::LOCAL, loader)
    }

    fn load_command_values_with_policy<F>(
        &self,
        kind: DynamicCommandCacheKind,
        scope_dir: PathBuf,
        query_policy: CommandQueryPolicy,
        loader: F,
    ) -> Vec<String>
    where
        F: FnOnce() -> Result<Vec<String>> + Send + 'static,
    {
        let cache_key = DynamicCommandCacheKey { kind, scope_dir };

        {
            let mut cache = self.cache.write();
            if let Some(entry) = cache.commands.get(&cache_key) {
                let values = entry.values.clone();
                let retry_allowed = cache
                    .command_errors
                    .get(&cache_key)
                    .is_none_or(|error| error.recorded_at.elapsed() >= query_policy.error_backoff);
                let start_refresh = entry.cached_at.elapsed() >= query_policy.ttl
                    && retry_allowed
                    && cache.command_pending.insert(cache_key.clone());
                update_diagnostics_from_cache(&self.runtime, &cache, None);
                drop(cache);
                if start_refresh {
                    self.mark_refresh_scheduled();
                    spawn_command_refresh(
                        self.runtime.clone(),
                        self.cache.clone(),
                        cache_key,
                        loader,
                    );
                }
                return values;
            }

            if cache
                .command_errors
                .get(&cache_key)
                .is_some_and(|error| error.recorded_at.elapsed() < query_policy.error_backoff)
            {
                update_diagnostics_from_cache(&self.runtime, &cache, None);
                return Vec::new();
            }

            if !cache.command_pending.insert(cache_key.clone()) {
                update_diagnostics_from_cache(&self.runtime, &cache, None);
                return Vec::new();
            }
            update_diagnostics_from_cache(&self.runtime, &cache, None);
        }

        self.mark_refresh_scheduled();
        spawn_command_refresh(self.runtime.clone(), self.cache.clone(), cache_key, loader);
        Vec::new()
    }

    fn lookup_command_values(
        &self,
        kind: DynamicCommandCacheKind,
        scope_dir: PathBuf,
    ) -> Vec<String> {
        let cache_key = DynamicCommandCacheKey { kind, scope_dir };
        self.cache
            .read()
            .commands
            .get(&cache_key)
            .map(|entry| entry.values.clone())
            .unwrap_or_default()
    }

    fn load_external_candidates<F>(
        &self,
        cache_key: ExternalCompletionCacheKey,
        loader: F,
    ) -> Result<Vec<EnhancedCandidate>>
    where
        F: FnOnce() -> Result<Vec<EnhancedCandidate>> + Send + 'static,
    {
        let ttl = Duration::from_millis(DYNAMIC_COMMAND_CACHE_TTL_MS);
        let mut start_refresh = false;

        {
            let mut cache = self.cache.write();
            if let Some(entry) = cache.external.get(&cache_key) {
                let candidates = entry.candidates.clone();
                if entry.cached_at.elapsed() >= ttl
                    && cache.external_pending.insert(cache_key.clone())
                {
                    start_refresh = true;
                }
                update_diagnostics_from_cache(&self.runtime, &cache, None);
                drop(cache);
                if start_refresh {
                    self.mark_refresh_scheduled();
                    spawn_external_refresh(
                        self.runtime.clone(),
                        self.cache.clone(),
                        cache_key,
                        loader,
                    );
                }
                return Ok(candidates);
            }

            if !cache.external_pending.insert(cache_key.clone()) {
                update_diagnostics_from_cache(&self.runtime, &cache, None);
                return Ok(Vec::new());
            }
            update_diagnostics_from_cache(
                &self.runtime,
                &cache,
                Some("external initial-load".to_string()),
            );
        }

        self.mark_refresh_scheduled();
        spawn_external_refresh(self.runtime.clone(), self.cache.clone(), cache_key, loader);
        Ok(Vec::new())
    }

    fn resolve_command_path(&self, command_name: &str) -> Option<String> {
        self.environment.read().lookup(command_name)
    }
}

#[cfg(test)]
mod tests;
