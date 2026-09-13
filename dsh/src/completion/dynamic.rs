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
mod container;
mod dev;
mod external;
mod git;
mod kubernetes;
mod linux;
mod local;
mod platform;
mod project;
mod registry;
mod runner;
mod runtime;
mod specs;
mod worker;

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

    pub(crate) fn collect_pacman_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let Some(sync) = pacman_sync_mode(parsed_command_line) else {
            return Vec::new();
        };
        self.collect_pacman_package_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            sync,
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

    fn collect_pacman_package_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        sync: bool,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let (kind, args, description) = if sync {
            ("sync-package", vec!["-Slq"], "pacman sync package")
        } else {
            ("installed-package", vec!["-Qq"], "installed pacman package")
        };
        let command_path = self.resolve_command_path("pacman");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "pacman",
            kind,
            canonicalize_path(&current_dir),
            current_token,
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

    fn collect_apt_installed_package_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        command_name: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("dpkg-query");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            command_name,
            "installed-package",
            PathBuf::from("/var/lib/dpkg/status"),
            current_token,
            "installed deb package",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_package_lines(&run_command_lines(
                    &command_path,
                    &["-W", "-f=${binary:Package}\\n"],
                    &current_dir,
                )?))
            },
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

    /// Homebrew-installed formulae and casks (`brew list`), for
    /// `brew uninstall`/`brew upgrade` completion. Global (no project scope).
    fn collect_brew_installed_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("brew");
        // brew is machine-global; use a fixed scope so the cache is shared
        // across working directories.
        let scope_dir = PathBuf::from("/");
        self.collect_cached_value_candidates(
            "brew",
            "installed",
            scope_dir,
            current_token,
            "brew installed",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let mut values =
                    run_command_lines(&command_path, &["list", "--formula"], Path::new("/"))?;
                if let Ok(casks) =
                    run_command_lines(&command_path, &["list", "--cask"], Path::new("/"))
                {
                    values.extend(casks);
                }
                Ok(dedup_sorted(values))
            },
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

fn canonicalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn spawn_command_refresh<F>(
    runtime: Arc<CompletionRuntime>,
    cache: Arc<RwLock<ProjectDynamicCache>>,
    cache_key: DynamicCommandCacheKey,
    loader: F,
) where
    F: FnOnce() -> Result<Vec<String>> + Send + 'static,
{
    let rejected_cache = cache.clone();
    let rejected_key = cache_key.clone();
    let job_runtime = runtime.clone();
    if !runtime.submit_command(Box::new(move || {
        let load_started = Instant::now();
        let result = loader();
        let load_duration = load_started.elapsed();
        let mut cache = cache.write();
        cache.command_pending.remove(&cache_key);
        match result {
            Ok(values) => {
                cache.command_errors.remove(&cache_key);
                cache.commands.insert(
                    cache_key,
                    CommandValueCacheEntry {
                        values,
                        cached_at: Instant::now(),
                        last_load_duration: Some(load_duration),
                        last_error: None,
                    },
                );
                prune_command_cache(&mut cache);
                update_diagnostics_from_cache(&job_runtime, &cache, None);
                job_runtime.notify();
            }
            Err(err) => {
                warn!("Dynamic command completion refresh failed: {}", err);
                cache.command_errors.insert(
                    cache_key,
                    CommandValueErrorEntry {
                        recorded_at: Instant::now(),
                        last_load_duration: load_duration,
                        error: err.to_string(),
                    },
                );
                prune_command_error_cache(&mut cache);
                update_diagnostics_from_cache(&job_runtime, &cache, None);
            }
        }
    })) {
        let mut cache = rejected_cache.write();
        cache.command_pending.remove(&rejected_key);
        runtime.record_queue_drop("dynamic command");
        update_diagnostics_from_cache(&runtime, &cache, None);
    }
}

fn spawn_external_refresh<F>(
    runtime: Arc<CompletionRuntime>,
    cache: Arc<RwLock<ProjectDynamicCache>>,
    cache_key: ExternalCompletionCacheKey,
    loader: F,
) where
    F: FnOnce() -> Result<Vec<EnhancedCandidate>> + Send + 'static,
{
    let is_fish = cache_key.command_template.starts_with("fish-fallback:");
    let rejected_cache = cache.clone();
    let rejected_key = cache_key.clone();
    let job_runtime = runtime.clone();
    if !runtime.submit_external(
        is_fish,
        Box::new(move || {
            let result = loader();
            let mut cache = cache.write();
            cache.external_pending.remove(&cache_key);
            match result {
                Ok(candidates) => {
                    if candidates.is_empty() {
                        update_diagnostics_from_cache(
                            &job_runtime,
                            &cache,
                            Some("external refresh empty".to_string()),
                        );
                    } else {
                        insert_external_cache_entry(
                            &mut cache,
                            cache_key,
                            ExternalCompletionCacheEntry {
                                candidates,
                                cached_at: Instant::now(),
                            },
                        );
                        update_diagnostics_from_cache(
                            &job_runtime,
                            &cache,
                            Some("external refresh ok".to_string()),
                        );
                        job_runtime.notify();
                    }
                }
                Err(err) => {
                    warn!("External completer refresh failed: {}", err);
                    update_diagnostics_from_cache(
                        &job_runtime,
                        &cache,
                        Some(format!("external refresh error: {err}")),
                    );
                }
            }
        }),
    ) {
        let mut cache = rejected_cache.write();
        cache.external_pending.remove(&rejected_key);
        runtime.record_queue_drop(if is_fish { "fish" } else { "external" });
        update_diagnostics_from_cache(
            &runtime,
            &cache,
            Some("external refresh dropped: queue full".to_string()),
        );
    }
}

fn insert_external_cache_entry(
    cache: &mut ProjectDynamicCache,
    cache_key: ExternalCompletionCacheKey,
    entry: ExternalCompletionCacheEntry,
) {
    cache.external.insert(cache_key, entry);
    prune_external_cache(cache);
}

fn prune_command_cache(cache: &mut ProjectDynamicCache) {
    let overflow = cache
        .commands
        .len()
        .saturating_sub(DYNAMIC_COMMAND_CACHE_LIMIT);
    if overflow == 0 {
        return;
    }

    let mut keys = cache
        .commands
        .iter()
        .filter(|(key, _)| !cache.command_pending.contains(*key))
        .map(|(key, entry)| (key.clone(), entry.cached_at))
        .collect::<Vec<_>>();
    keys.sort_by_key(|(_, cached_at)| *cached_at);

    for (key, _) in keys.into_iter().take(overflow) {
        if cache.commands.remove(&key).is_some() {
            cache.command_errors.remove(&key);
            cache.command_pruned_total += 1;
        }
    }
}

fn prune_command_error_cache(cache: &mut ProjectDynamicCache) {
    let overflow = cache
        .command_errors
        .len()
        .saturating_sub(DYNAMIC_COMMAND_CACHE_LIMIT);
    if overflow == 0 {
        return;
    }

    let mut keys = cache
        .command_errors
        .iter()
        .filter(|(key, _)| !cache.command_pending.contains(*key))
        .map(|(key, entry)| (key.clone(), entry.recorded_at))
        .collect::<Vec<_>>();
    keys.sort_by_key(|(_, recorded_at)| *recorded_at);

    for (key, _) in keys.into_iter().take(overflow) {
        if cache.command_errors.remove(&key).is_some() {
            cache.command_pruned_total += 1;
        }
    }
}

fn prune_external_cache(cache: &mut ProjectDynamicCache) {
    let overflow = cache
        .external
        .len()
        .saturating_sub(EXTERNAL_COMPLETION_CACHE_LIMIT);
    if overflow == 0 {
        return;
    }

    let mut keys = cache
        .external
        .iter()
        .map(|(key, entry)| (key.clone(), entry.cached_at))
        .collect::<Vec<_>>();
    keys.sort_by_key(|(_, cached_at)| *cached_at);

    for (key, _) in keys.into_iter().take(overflow) {
        if cache.external.remove(&key).is_some() {
            cache.external_pruned_total += 1;
        }
    }
}

fn update_diagnostics_from_cache(
    runtime: &CompletionRuntime,
    cache: &ProjectDynamicCache,
    last_external: Option<String>,
) {
    let mut diagnostics = runtime.diagnostics.write();
    diagnostics.command_entries = cache.commands.len();
    diagnostics.command_pending = cache.command_pending.len();
    diagnostics.command_pruned_total = cache.command_pruned_total;
    diagnostics.external_entries = cache.external.len();
    diagnostics.external_pending = cache.external_pending.len();
    diagnostics.external_fish_entries = cache
        .external
        .keys()
        .filter(|key| key.command_template.starts_with("fish-fallback:"))
        .count();
    diagnostics.external_pruned_total = cache.external_pruned_total;
    diagnostics.last_refresh = Some(Instant::now());
    diagnostics.provider_lines = provider_diagnostics_lines(cache);
    if let Some(last_external) = last_external {
        diagnostics.last_external = Some(last_external);
    }
}

fn provider_diagnostics_lines(cache: &ProjectDynamicCache) -> Vec<String> {
    let mut keys = cache
        .commands
        .keys()
        .chain(cache.command_errors.keys())
        .chain(cache.command_pending.iter())
        .cloned()
        .collect::<Vec<_>>();
    keys.sort_by(|a, b| {
        dynamic_cache_kind_label(&a.kind)
            .cmp(&dynamic_cache_kind_label(&b.kind))
            .then_with(|| a.scope_dir.cmp(&b.scope_dir))
    });
    keys.dedup();
    keys.into_iter()
        .take(12)
        .map(|key| {
            let entry = cache.commands.get(&key);
            let error = cache.command_errors.get(&key);
            let pending = cache.command_pending.contains(&key);
            let values = entry.map(|entry| entry.values.len()).unwrap_or(0);
            let age = entry
                .map(|entry| format!("{}ms", entry.cached_at.elapsed().as_millis()))
                .or_else(|| error.map(|entry| format!("{}ms", entry.recorded_at.elapsed().as_millis())))
                .unwrap_or_else(|| "none".to_string());
            let duration = entry
                .and_then(|entry| entry.last_load_duration)
                .or_else(|| error.map(|entry| entry.last_load_duration))
                .map(|duration| format!("{}ms", duration.as_millis()))
                .unwrap_or_else(|| "unknown".to_string());
            let error_text = entry
                .and_then(|entry| entry.last_error.clone())
                .or_else(|| error.map(|entry| truncate_string(&entry.error, 80)))
                .unwrap_or_else(|| "none".to_string());
            format!(
                "completion-cache provider {} values={} pending={} age={} last-duration={} error={}",
                dynamic_cache_kind_label(&key.kind),
                values,
                pending,
                age,
                duration,
                error_text
            )
        })
        .collect()
}

/// The pacman operation the command line selects, as its single upper-case
/// letter.
///
/// pacman spells operations as short flags that are routinely bundled with
/// their modifiers (`-Rns`, `-Syu`, `-Qi`) or written out in long form
/// (`--remove`). Matching `-R`/`-S` literally, as this used to, left every
/// bundled form with no candidates at all.
fn pacman_operation(parsed_command_line: &ParsedCommandLine) -> Option<char> {
    const OPERATIONS: [char; 7] = ['S', 'R', 'Q', 'U', 'F', 'D', 'T'];

    parsed_command_line
        .subcommand_path
        .iter()
        .chain(parsed_command_line.raw_args.iter())
        // The token under the cursor is still being typed: `pacman -R<TAB>` is
        // completing the flag itself, not a package name for it.
        .filter(|token| token.as_str() != parsed_command_line.current_token)
        .find_map(|token| match token.as_str() {
            "--sync" => Some('S'),
            "--remove" => Some('R'),
            "--query" => Some('Q'),
            "--upgrade" => Some('U'),
            "--files" => Some('F'),
            "--database" => Some('D'),
            "--deptest" => Some('T'),
            value if value.starts_with("--") => None,
            value => value
                .strip_prefix('-')
                .and_then(|flags| flags.chars().next())
                .filter(|flag| OPERATIONS.contains(flag)),
        })
}

/// Whether pacman package candidates should come from the sync repositories
/// (`true`, for installs) or from the local database (`false`).
///
/// Only the local database lists AUR/foreign packages, so every operation that
/// acts on already-installed packages must land on `false`. `None` means the
/// operation takes no package name and should offer nothing.
pub(crate) fn pacman_sync_mode(parsed_command_line: &ParsedCommandLine) -> Option<bool> {
    match pacman_operation(parsed_command_line)? {
        'S' => Some(true),
        'R' | 'Q' | 'F' | 'D' | 'T' => Some(false),
        _ => None,
    }
}

fn cached_value_matches(values: Vec<String>, current_token: &str) -> Vec<String> {
    if current_token.is_empty() {
        return values;
    }

    let mut prefix_matches = Vec::new();
    let mut fuzzy_candidates = Vec::new();
    for value in values {
        if value.starts_with(current_token) {
            prefix_matches.push(value);
        } else {
            fuzzy_candidates.push(value);
        }
    }

    if !prefix_matches.is_empty() {
        return prefix_matches;
    }

    fuzzy_candidates
        .into_iter()
        .filter(|value| matches_prefix(current_token, value))
        .collect()
}

fn dynamic_cache_kind_label(kind: &DynamicCommandCacheKind) -> String {
    match kind {
        DynamicCommandCacheKind::GitBranch => "git.branch".to_string(),
        DynamicCommandCacheKind::GitRemote => "git.remote".to_string(),
        DynamicCommandCacheKind::GitWorktree => "git.worktree".to_string(),
        DynamicCommandCacheKind::KubectlContext => "kubectl.context".to_string(),
        DynamicCommandCacheKind::KubectlNamespace => "kubectl.namespace".to_string(),
        DynamicCommandCacheKind::CommandValue {
            command,
            value_kind,
        } => format!("{command}.{value_kind}"),
    }
}

fn file_metadata_signature(path: &Path) -> FileMetadataSignature {
    match fs::metadata(path) {
        Ok(metadata) => FileMetadataSignature {
            exists: true,
            modified: metadata.modified().ok(),
            len: metadata.len(),
        },
        Err(_) => FileMetadataSignature {
            exists: false,
            modified: None,
            len: 0,
        },
    }
}

fn task_completion_signature(
    project_root: &Path,
    sources: Option<&[&str]>,
) -> Vec<FileMetadataSignature> {
    let mut paths = [
        "mise.toml",
        "Taskfile.yml",
        "Taskfile.yaml",
        "turbo.json",
        "package.json",
        "Cargo.toml",
        "Makefile",
        "makefile",
        "deno.json",
        "deno.jsonc",
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "gradle.properties",
        "gradlew",
    ]
    .into_iter()
    .map(|name| project_root.join(name))
    .collect::<Vec<_>>();

    if sources_include_nx(sources) {
        paths.extend([
            project_root.join("workspace.json"),
            project_root.join("angular.json"),
            project_root.join("project.json"),
        ]);
        paths.extend(descendant_project_json_files(project_root, 4));
    }
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| file_metadata_signature(&path))
        .collect()
}

fn sources_include_nx(sources: Option<&[&str]>) -> bool {
    sources.is_none_or(|sources| sources.contains(&"nx"))
}

fn descendant_project_json_files(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_descendant_project_json_files(root, 0, max_depth, &mut paths);
    paths
}

fn collect_descendant_project_json_files(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    paths: &mut Vec<PathBuf>,
) {
    if depth > max_depth {
        return;
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') || matches!(name, "node_modules" | "target" | "dist" | "build") {
            continue;
        }
        if path.is_file() && name == "project.json" {
            paths.push(path);
        } else if path.is_dir() {
            collect_descendant_project_json_files(&path, depth + 1, max_depth, paths);
        }
    }
}

fn normalized_task_sources(sources: &[&str]) -> Vec<String> {
    let mut sources = sources
        .iter()
        .map(|source| (*source).to_string())
        .collect::<Vec<_>>();
    sources.sort();
    sources.dedup();
    sources
}

fn find_compose_file(current_dir: &Path) -> Option<PathBuf> {
    const CANDIDATES: [&str; 4] = [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
    ];

    current_dir.ancestors().find_map(|dir| {
        CANDIDATES
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.exists())
    })
}

fn selected_docker_compose_command(parsed_command_line: &ParsedCommandLine) -> Option<&str> {
    let mut skip_next_value = false;

    for token in docker_compose_words(parsed_command_line) {
        if skip_next_value {
            skip_next_value = false;
            continue;
        }

        if docker_compose_option_takes_value(token) {
            skip_next_value = true;
            continue;
        }

        if is_inline_docker_compose_option_value(token) || token.starts_with('-') {
            continue;
        }

        return Some(token);
    }

    None
}

fn selected_docker_compose_file(
    parsed_command_line: &ParsedCommandLine,
    current_dir: &Path,
) -> Option<PathBuf> {
    let words = docker_compose_words(parsed_command_line);

    for (index, token) in words.iter().enumerate() {
        if *token == "-f" || *token == "--file" {
            let Some(value) = words.get(index + 1).copied() else {
                continue;
            };
            return compose_file_path_from_token(current_dir, value);
        }

        if let Some(value) = token
            .strip_prefix("--file=")
            .or_else(|| token.strip_prefix("-f="))
        {
            return compose_file_path_from_token(current_dir, value);
        }
    }

    None
}

fn docker_compose_words(parsed_command_line: &ParsedCommandLine) -> Vec<&str> {
    let words = completion_words(parsed_command_line);
    if parsed_command_line.command == "docker-compose" {
        return words;
    }

    if parsed_command_line.command == "docker" {
        let mut skip_next_value = false;
        let mut compose_index = None;
        for (index, word) in words.iter().enumerate() {
            if skip_next_value {
                skip_next_value = false;
                continue;
            }
            if docker_global_option_takes_value(word) {
                skip_next_value = true;
                continue;
            }
            if is_inline_docker_global_option_value(word) || word.starts_with('-') {
                continue;
            }
            if *word == "compose" {
                compose_index = Some(index);
                break;
            }
        }
        if let Some(index) = compose_index {
            return words.into_iter().skip(index + 1).collect();
        }
    }

    Vec::new()
}

fn compose_file_path_from_token(current_dir: &Path, token: &str) -> Option<PathBuf> {
    if token.is_empty() || token.starts_with('-') {
        return None;
    }

    let path = PathBuf::from(normalize_path_token(token));
    Some(if path.is_absolute() {
        path
    } else {
        current_dir.join(path)
    })
}

fn docker_compose_option_takes_value(token: &str) -> bool {
    matches!(
        token,
        "-f" | "--file"
            | "-p"
            | "--project-name"
            | "--profile"
            | "--env-file"
            | "--project-directory"
            | "--parallel"
    )
}

fn docker_global_option_takes_value(token: &str) -> bool {
    matches!(
        token,
        "-c" | "--config"
            | "--context"
            | "-H"
            | "--host"
            | "--log-level"
            | "--tlscacert"
            | "--tlscert"
            | "--tlskey"
    )
}

fn is_inline_docker_global_option_value(token: &str) -> bool {
    token.starts_with("--config=")
        || token.starts_with("-c=")
        || token.starts_with("--context=")
        || token.starts_with("-H=")
        || token.starts_with("--host=")
        || token.starts_with("--log-level=")
        || token.starts_with("--tlscacert=")
        || token.starts_with("--tlscert=")
        || token.starts_with("--tlskey=")
}

fn is_inline_docker_compose_option_value(token: &str) -> bool {
    token.starts_with("--file=")
        || token.starts_with("-f=")
        || token.starts_with("--project-name=")
        || token.starts_with("--profile=")
        || token.starts_with("--env-file=")
        || token.starts_with("--project-directory=")
        || token.starts_with("--parallel=")
}

fn parse_compose_service_names(path: &Path) -> Result<Vec<String>> {
    let contents = fs::read_to_string(path)?;
    let mut in_services = false;
    let mut services_indent = 0usize;
    let mut service_indent = None;
    let mut names = Vec::new();

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.chars().take_while(|c| c.is_whitespace()).count();
        if !in_services {
            if trimmed == "services:" {
                in_services = true;
                services_indent = indent;
            }
            continue;
        }

        if indent <= services_indent {
            break;
        }

        if trimmed.starts_with('-') {
            continue;
        }

        if !trimmed.ends_with(':') {
            continue;
        }

        let key = trimmed.trim_end_matches(':').trim();
        if key.is_empty() || key.contains(' ') {
            continue;
        }

        match service_indent {
            None => {
                service_indent = Some(indent);
                names.push(key.to_string());
            }
            Some(expected_indent) if indent == expected_indent => names.push(key.to_string()),
            _ => {}
        }
    }

    let mut seen = HashSet::new();
    names.retain(|name| seen.insert(name.clone()));
    Ok(names)
}

fn run_external_completer_for_key(
    key: &ExternalCompletionCacheKey,
) -> Result<Vec<EnhancedCandidate>> {
    let mut command = runner::shell_command(&key.command_template);
    command
        .current_dir(&key.current_dir)
        .env("DSH_COMPLETION_INPUT", &key.input)
        .env("DSH_COMPLETION_CURSOR", key.cursor_pos.to_string())
        .env("DSH_COMPLETION_COMMAND", &key.command)
        .env("DSH_COMPLETION_CURRENT_TOKEN", &key.current_token)
        .env("DSH_COMPLETION_SUBCOMMAND_PATH", &key.subcommand_path);

    let lines = collect_command_lines(command)?;
    Ok(lines
        .into_iter()
        .filter_map(|line| external::parse_line(&line, &key.current_token))
        .collect())
}

fn run_fish_completer_for_key(
    command_path: &str,
    key: &ExternalCompletionCacheKey,
) -> Result<Vec<EnhancedCandidate>> {
    let mut command = runner::command(command_path);
    command
        .arg("-c")
        .arg("complete -C \"$argv[1]\"")
        .arg("--")
        .arg(&key.input)
        .current_dir(&key.current_dir);

    let lines = collect_command_lines(command)?;
    Ok(lines
        .into_iter()
        .filter_map(|line| external::parse_fish_line(&line, &key.current_token))
        .collect())
}

fn run_command_stdout(command_path: &str, args: &[&str], current_dir: &Path) -> Result<String> {
    let mut command = runner::command(command_path);
    command.args(args).current_dir(current_dir);
    runner::collect_stdout(command)
}

fn run_command_lines(command_path: &str, args: &[&str], current_dir: &Path) -> Result<Vec<String>> {
    let mut command = runner::command(command_path);
    command.args(args).current_dir(current_dir);
    collect_command_lines(command)
}

fn collect_command_lines(command: std::process::Command) -> Result<Vec<String>> {
    Ok(runner::collect_stdout(command)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn dedup_sorted(mut values: Vec<String>) -> Vec<String> {
    values.retain(|value| !value.trim().is_empty());
    values.sort();
    values.dedup();
    values
}

fn parse_non_empty_lines(lines: &[String]) -> Vec<String> {
    dedup_sorted(lines.iter().map(|line| line.trim().to_string()).collect())
}

fn shell_state_candidates(
    values: Vec<String>,
    current_token: &str,
    description: &str,
) -> Vec<EnhancedCandidate> {
    dedup_sorted(values)
        .into_iter()
        .filter(|value| matches_prefix(current_token, value))
        .map(|value| EnhancedCandidate {
            text: value,
            description: Some(description.to_string()),
            candidate_type: CandidateType::Argument,
            priority: 140,
        })
        .collect()
}

fn parse_first_fields(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split_whitespace().next().map(str::to_string))
            .collect(),
    )
}

fn parse_first_column_lines(lines: &[String]) -> Vec<String> {
    parse_first_fields(lines)
        .into_iter()
        .filter(|value| !value.eq_ignore_ascii_case("name"))
        .collect()
}

fn parse_minikube_profiles(output: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    let mut profiles = Vec::new();
    for group in ["valid", "invalid"] {
        let Some(entries) = value.get(group).and_then(serde_json::Value::as_array) else {
            continue;
        };
        for entry in entries {
            if let Some(name) = entry
                .get("Name")
                .or_else(|| entry.get("name"))
                .and_then(serde_json::Value::as_str)
            {
                profiles.push(name.to_string());
            }
        }
    }
    dedup_sorted(profiles)
}

fn parse_whitespace_values(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .flat_map(|line| line.split_whitespace())
            .map(str::to_string)
            .collect(),
    )
}

fn parse_journalctl_boots(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let offset = fields.next()?;
                if offset.parse::<i32>().is_ok() {
                    Some(offset.to_string())
                } else {
                    fields.next().map(str::to_string)
                }
            })
            .collect(),
    )
}

fn parse_networkctl_links(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let first = fields.next()?;
                if first == "IDX" {
                    return None;
                }
                if first.parse::<u32>().is_ok() {
                    fields.next().map(str::to_string)
                } else {
                    Some(first.to_string())
                }
            })
            .collect(),
    )
}

fn completion_words(parsed_command_line: &ParsedCommandLine) -> Vec<&str> {
    parsed_command_line
        .subcommand_path
        .iter()
        .chain(parsed_command_line.raw_args.iter())
        .map(String::as_str)
        .collect()
}

fn tar_reads_archive(parsed_command_line: &ParsedCommandLine) -> bool {
    completion_words(parsed_command_line)
        .into_iter()
        .any(|word| {
            matches!(
                word,
                "-x" | "--extract" | "--get" | "-t" | "--list" | "x" | "t"
            ) || word
                .strip_prefix('-')
                .filter(|flags| !flags.starts_with('-'))
                .is_some_and(|flags| flags.contains('x') || flags.contains('t'))
        })
}

fn selected_tar_archive(
    parsed_command_line: &ParsedCommandLine,
    current_dir: &Path,
) -> Option<PathBuf> {
    let words = completion_words(parsed_command_line);
    for (index, word) in words.iter().enumerate() {
        if let Some(value) = word.strip_prefix("--file=") {
            return archive_path_from_token(current_dir, value);
        }
        if matches!(*word, "-f" | "--file") {
            return words
                .get(index + 1)
                .and_then(|value| archive_path_from_token(current_dir, value));
        }
        if word
            .strip_prefix('-')
            .filter(|flags| !flags.starts_with('-'))
            .is_some_and(|flags| flags.contains('f'))
            || (!word.starts_with('-')
                && word.chars().all(|ch| ch.is_ascii_alphabetic())
                && word.contains('f')
                && (word.contains('x') || word.contains('t')))
        {
            return words
                .get(index + 1)
                .and_then(|value| archive_path_from_token(current_dir, value));
        }
    }
    None
}

fn selected_unzip_archive(
    parsed_command_line: &ParsedCommandLine,
    current_dir: &Path,
) -> Option<PathBuf> {
    parsed_command_line
        .specified_arguments
        .first()
        .filter(|value| !value.is_empty() && *value != &parsed_command_line.current_token)
        .and_then(|value| archive_path_from_token(current_dir, value))
}

fn archive_path_from_token(current_dir: &Path, token: &str) -> Option<PathBuf> {
    if token.is_empty() || token.starts_with('-') {
        return None;
    }
    let path = PathBuf::from(normalize_path_token(token));
    Some(if path.is_absolute() {
        path
    } else {
        current_dir.join(path)
    })
}

fn archive_file_candidates(current_token: &str) -> Vec<EnhancedCandidate> {
    FileSystemGenerator::generate_file_candidates(current_token)
        .unwrap_or_default()
        .into_iter()
        .map(|candidate| {
            let candidate_type = match candidate.completion_type {
                CompletionType::Directory => CandidateType::Directory,
                _ => CandidateType::File,
            };
            EnhancedCandidate {
                text: candidate.text,
                description: candidate.description,
                candidate_type,
                priority: candidate.priority,
            }
        })
        .collect()
}

fn systemctl_unit_kind_for_context(parsed_command_line: &ParsedCommandLine) -> SystemdUnitListKind {
    parsed_command_line
        .subcommand_path
        .first()
        .and_then(|subcommand| systemctl_unit_kind_for_subcommand(subcommand))
        .unwrap_or(SystemdUnitListKind::All)
}

fn systemctl_unit_kind_for_subcommand(subcommand: &str) -> Option<SystemdUnitListKind> {
    match subcommand {
        "start" => Some(SystemdUnitListKind::UnitFiles),
        "stop" | "restart" | "reload" => Some(SystemdUnitListKind::Running),
        "enable" => Some(SystemdUnitListKind::Disabled),
        "disable" => Some(SystemdUnitListKind::Enabled),
        "status" | "is-active" | "is-enabled" | "mask" | "unmask" => Some(SystemdUnitListKind::All),
        _ => None,
    }
}

/// Maps a `systemctl.unit` / `systemctl.unit_file` provider scope onto the
/// matching `systemctl --type=` filter, so a JSON definition can narrow the
/// candidates to timers, sockets, slices and so on.
fn systemd_unit_type_filter(scope: Option<&str>) -> Option<&'static str> {
    match scope? {
        "service" => Some("--type=service"),
        "socket" => Some("--type=socket"),
        "timer" => Some("--type=timer"),
        "slice" => Some("--type=slice"),
        "target" => Some("--type=target"),
        "mount" => Some("--type=mount"),
        "automount" => Some("--type=automount"),
        "path" => Some("--type=path"),
        "swap" => Some("--type=swap"),
        "scope" => Some("--type=scope"),
        "device" => Some("--type=device"),
        _ => None,
    }
}

fn selected_systemd_manager_scope(
    parsed_command_line: &ParsedCommandLine,
) -> Option<SystemdManagerScope> {
    if matches!(
        &parsed_command_line.completion_context,
        CompletionContext::OptionValue { option_name, .. } if option_name == "--user-unit"
    ) {
        return Some(SystemdManagerScope::User);
    }

    let has_option = |name: &str| {
        parsed_command_line
            .specified_options
            .iter()
            .chain(parsed_command_line.raw_args.iter())
            .any(|token| token == name)
    };
    let has_inline_option_value = |name: &str| {
        parsed_command_line.raw_args.iter().any(|token| {
            token
                .strip_prefix(name)
                .is_some_and(|suffix| suffix.starts_with('='))
        })
    };

    if has_option("--user-unit") || has_inline_option_value("--user-unit") || has_option("--user") {
        Some(SystemdManagerScope::User)
    } else if has_option("--global") {
        Some(SystemdManagerScope::Global)
    } else if has_option("--system") {
        Some(SystemdManagerScope::System)
    } else {
        None
    }
}

fn input_prefix_at_cursor(input: &str, cursor_pos: usize) -> String {
    input.chars().take(cursor_pos).collect()
}

fn env_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn env_falsey(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

fn parse_cargo_metadata_values(output: &str, kind: CargoMetadataValueKind) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };

    let mut values = Vec::new();
    let Some(packages) = value
        .get("packages")
        .and_then(|packages| packages.as_array())
    else {
        return Vec::new();
    };

    for package in packages {
        match kind {
            CargoMetadataValueKind::Package => {
                if let Some(name) = package.get("name").and_then(|name| name.as_str()) {
                    values.push(name.to_string());
                }
            }
            CargoMetadataValueKind::Feature => {
                if let Some(features) = package
                    .get("features")
                    .and_then(|features| features.as_object())
                {
                    values.extend(features.keys().cloned());
                }
            }
            CargoMetadataValueKind::Bin
            | CargoMetadataValueKind::Example
            | CargoMetadataValueKind::Test
            | CargoMetadataValueKind::Bench => {
                let Some(targets) = package
                    .get("targets")
                    .and_then(|targets| targets.as_array())
                else {
                    continue;
                };
                let expected_kind = match kind {
                    CargoMetadataValueKind::Bin => "bin",
                    CargoMetadataValueKind::Example => "example",
                    CargoMetadataValueKind::Test => "test",
                    CargoMetadataValueKind::Bench => "bench",
                    CargoMetadataValueKind::Package | CargoMetadataValueKind::Feature => {
                        unreachable!()
                    }
                };
                for target in targets {
                    let Some(kinds) = target.get("kind").and_then(|kinds| kinds.as_array()) else {
                        continue;
                    };
                    let has_kind = kinds
                        .iter()
                        .any(|target_kind| target_kind.as_str() == Some(expected_kind));
                    if has_kind
                        && let Some(name) = target.get("name").and_then(|name| name.as_str())
                    {
                        values.push(name.to_string());
                    }
                }
            }
        }
    }

    dedup_sorted(values)
}

fn cargo_feature_token_parts(token: &str) -> (&str, &str) {
    token
        .rfind(',')
        .map(|comma| token.split_at(comma + 1))
        .unwrap_or(("", token))
}

fn ssh_config_scope() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".ssh"))
        .unwrap_or_else(|| PathBuf::from(".ssh"))
}

fn load_ssh_hosts() -> Vec<String> {
    let mut values = Vec::new();
    if let Some(home) = dirs::home_dir() {
        values.extend(parse_ssh_config_hosts(
            &fs::read_to_string(home.join(".ssh").join("config")).unwrap_or_default(),
        ));
        values.extend(parse_known_hosts(
            &fs::read_to_string(home.join(".ssh").join("known_hosts")).unwrap_or_default(),
        ));
    }
    dedup_sorted(values)
}

fn man_page_roots(configured_manpath: Option<&str>) -> Vec<PathBuf> {
    let mut roots = configured_manpath
        .filter(|value| !value.trim().is_empty())
        .map(|value| std::env::split_paths(value).collect::<Vec<_>>())
        .unwrap_or_default();
    roots.extend([
        PathBuf::from("/usr/local/share/man"),
        PathBuf::from("/usr/share/man"),
    ]);
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".local/share/man"));
    }
    roots.sort();
    roots.dedup();
    roots.retain(|path| path.is_dir());
    roots
}

fn load_man_page_names(roots: &[PathBuf]) -> Vec<String> {
    let mut values = Vec::new();
    for root in roots {
        collect_man_page_names(root, 0, &mut values);
    }
    dedup_sorted(values)
}

fn collect_man_page_names(dir: &Path, depth: usize, values: &mut Vec<String>) {
    if depth > 2 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_man_page_names(&path, depth + 1, values);
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(page) = man_page_name_from_file(file_name) {
            values.push(page.to_string());
        }
    }
}

fn man_page_name_from_file(file_name: &str) -> Option<&str> {
    let mut stem = file_name;
    for extension in [".gz", ".xz", ".bz2", ".zst", ".lzma"] {
        if let Some(stripped) = stem.strip_suffix(extension) {
            stem = stripped;
            break;
        }
    }
    let (page, section) = stem.rsplit_once('.')?;
    (!page.is_empty() && !section.is_empty()).then_some(page)
}

/// `user` and `user:group` values for `chown` and `chgrp`.
///
/// Both sides come from the generators, which read `/etc/passwd` and
/// `/etc/group` on Linux and Open Directory on macOS; parsing the files here
/// too offered nothing but service accounts on macOS for owners, and missed
/// every directory-managed group for groups. Service accounts are wanted in
/// this list -- both `chown www-data` and `chown _www` are ordinary -- so
/// nothing is filtered out.
fn load_owner_group_values() -> Vec<String> {
    let mut values = Vec::new();
    values.extend(
        crate::completion::generators::user::user_names(true)
            .into_iter()
            .map(|name| format!("u:{name}")),
    );
    values.extend(
        crate::completion::generators::group::group_names()
            .into_iter()
            .map(|name| format!("g:{name}")),
    );
    dedup_sorted(values)
}

fn owner_group_candidates(values: &[String], current_token: &str) -> Vec<EnhancedCandidate> {
    let group_context = current_token.rsplit_once(':');
    values
        .iter()
        .filter_map(|encoded| {
            let (kind, value) = encoded.split_once(':')?;
            let (text, description) = if let Some((owner, group_prefix)) = group_context {
                if kind != "g" || !matches_prefix(group_prefix, value) {
                    return None;
                }
                (format!("{owner}:{value}"), "group")
            } else {
                if kind != "u" || !matches_prefix(current_token, value) {
                    return None;
                }
                (value.to_string(), "user")
            };
            Some(EnhancedCandidate {
                text,
                description: Some(description.to_string()),
                candidate_type: CandidateType::Argument,
                priority: 140,
            })
        })
        .collect()
}

fn parse_ssh_config_hosts(contents: &str) -> Vec<String> {
    let mut values = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        if !parts
            .next()
            .is_some_and(|keyword| keyword.eq_ignore_ascii_case("host"))
        {
            continue;
        }
        for host in parts {
            if host.contains('*') || host.contains('?') || host.starts_with('!') {
                continue;
            }
            values.push(host.to_string());
        }
    }
    dedup_sorted(values)
}

fn parse_known_hosts(contents: &str) -> Vec<String> {
    let mut values = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() || trimmed.starts_with('|') {
            continue;
        }
        let fields = trimmed.split_whitespace().collect::<Vec<_>>();
        let host_field = if fields.first().is_some_and(|field| field.starts_with('@')) {
            fields.get(1).copied()
        } else {
            fields.first().copied()
        };
        let Some(host_field) = host_field else {
            continue;
        };
        for host in host_field.split(',') {
            let host = if let Some(rest) = host.strip_prefix('[') {
                rest.split(']').next().unwrap_or(rest)
            } else {
                host.split(':').next().unwrap_or(host)
            };
            if !host.is_empty() && !host.starts_with('|') {
                values.push(host.to_string());
            }
        }
    }
    dedup_sorted(values)
}

fn format_ssh_host_candidate_text(
    command_name: &str,
    user_prefix: Option<&str>,
    host: String,
) -> String {
    let mut text = if let Some(user) = user_prefix {
        format!("{user}@{host}")
    } else {
        host
    };
    if matches!(command_name, "scp" | "rsync") {
        text.push(':');
    }
    text
}

fn parse_screen_sessions(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .flat_map(|line| line.split_whitespace())
            .filter(|field| {
                field.split_once('.').is_some_and(|(pid, name)| {
                    !name.is_empty() && pid.chars().all(|ch| ch.is_ascii_digit())
                })
            })
            .map(str::to_string)
            .collect(),
    )
}

/// Running process names, for `pkill`/`killall`-style completion.
///
/// Delegates to the process generator for the same reason
/// [`load_network_interfaces`] does: this walked `/proc` itself and so came
/// back empty on macOS while the generator had a working source.
fn load_process_names() -> Vec<String> {
    dedup_sorted(crate::completion::generators::process::process_names())
}

/// Running pids. See [`load_process_names`].
fn load_process_ids() -> Vec<String> {
    dedup_sorted(crate::completion::generators::process::process_ids())
}

fn parse_pip_freeze_packages(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split("==").next().map(str::to_string))
            .collect(),
    )
}

fn parse_package_lines(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
    )
}

fn parse_first_fields_excluding(lines: &[String], excluded: &[&str]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let first = line.split_whitespace().next()?;
                if first.is_empty()
                    || excluded
                        .iter()
                        .any(|header| first.eq_ignore_ascii_case(header))
                {
                    None
                } else {
                    Some(first.to_string())
                }
            })
            .collect(),
    )
}

fn parse_blkid_export_attribute(lines: &[String], attribute: &str) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let (key, value) = line.split_once('=')?;
                if key == attribute && !value.is_empty() {
                    Some(value.to_string())
                } else {
                    None
                }
            })
            .collect(),
    )
}

fn parse_busctl_services(lines: &[String]) -> Vec<String> {
    parse_first_fields_excluding(lines, &["NAME"])
}

fn parse_loginctl_sessions(lines: &[String]) -> Vec<String> {
    parse_first_fields_excluding(lines, &["SESSION"])
}

fn parse_loginctl_seats(lines: &[String]) -> Vec<String> {
    parse_first_fields_excluding(lines, &["SEAT"])
}

fn parse_losetup_devices(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        parse_first_fields_excluding(lines, &["NAME"])
            .into_iter()
            .map(|device| {
                if device.starts_with("/dev/") {
                    device
                } else {
                    format!("/dev/{device}")
                }
            })
            .collect(),
    )
}

fn parse_nmcli_first_field(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let first = line.split(':').next()?;
                if first.is_empty() {
                    None
                } else {
                    Some(first.to_string())
                }
            })
            .collect(),
    )
}

fn parse_nmcli_connected_devices(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let (device, state) = line.split_once(':')?;
                if !device.is_empty() && state == "connected" {
                    Some(device.to_string())
                } else {
                    None
                }
            })
            .collect(),
    )
}

fn parse_lsblk_devices(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let name = fields.next()?;
                let kind = fields.next()?;
                if matches!(kind, "disk" | "part" | "loop") {
                    Some(format!("/dev/{name}"))
                } else {
                    None
                }
            })
            .collect(),
    )
}

/// Filesystem types this kernel can mount, for `mount -t`.
#[cfg(not(target_os = "macos"))]
fn load_filesystem_types() -> Vec<String> {
    fs::read_to_string("/proc/filesystems")
        .map(|contents| {
            dedup_sorted(
                contents
                    .lines()
                    .filter_map(|line| line.split_whitespace().last().map(str::to_string))
                    .collect(),
            )
        })
        .unwrap_or_default()
}

/// macOS keeps one bundle per filesystem under `/System/Library/Filesystems`,
/// named `<type>.fs` -- `apfs.fs`, `msdos.fs`, `smbfs.fs` -- which is the same
/// vocabulary `mount -t` accepts. Entries without the suffix are helper
/// directories, not types.
#[cfg(target_os = "macos")]
fn load_filesystem_types() -> Vec<String> {
    fs::read_dir("/System/Library/Filesystems")
        .map(|entries| {
            dedup_sorted(
                entries
                    .flatten()
                    .filter_map(|entry| {
                        entry
                            .file_name()
                            .to_str()
                            .and_then(|name| name.strip_suffix(".fs"))
                            .map(str::to_string)
                    })
                    .collect(),
            )
        })
        .unwrap_or_default()
}

fn load_fstab_mountpoints() -> Vec<String> {
    fs::read_to_string("/etc/fstab")
        .map(|contents| parse_fstab_mountpoints(&contents))
        .unwrap_or_default()
}

fn parse_fstab_mountpoints(contents: &str) -> Vec<String> {
    dedup_sorted(
        contents
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                let mountpoint = line.split_whitespace().nth(1)?;
                Some(decode_fstab_field(mountpoint))
            })
            .collect(),
    )
}

fn decode_fstab_field(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

fn load_package_json_dependencies(package_json: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(package_json) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for key in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        if let Some(object) = value.get(key).and_then(|value| value.as_object()) {
            values.extend(object.keys().cloned());
        }
    }
    dedup_sorted(values)
}

/// Interface names for `tcpdump -i` and friends.
///
/// Delegates to the interface generator, which reads `/sys/class/net` where it
/// exists and `getifaddrs` on macOS. This used to read sysfs itself and so
/// returned nothing on macOS while the generator returned the real list.
fn load_network_interfaces() -> Vec<String> {
    dedup_sorted(crate::completion::generators::interface::interface_names())
}

fn load_swap_devices() -> Vec<String> {
    fs::read_to_string("/proc/swaps")
        .map(|contents| {
            dedup_sorted(
                contents
                    .lines()
                    .skip(1)
                    .filter_map(|line| line.split_whitespace().next().map(str::to_string))
                    .collect(),
            )
        })
        .unwrap_or_default()
}

/// Every tunable `sysctl` accepts.
///
/// `/proc/sys` is the same namespace with slashes for dots, so walking it
/// avoids spawning anything.
#[cfg(not(target_os = "macos"))]
fn load_sysctl_keys() -> Vec<String> {
    let root = Path::new("/proc/sys");
    let mut values = Vec::new();
    collect_sysctl_keys(root, root, &mut values);
    dedup_sorted(values)
}

/// macOS exposes the same namespace only through the tool itself: there is no
/// procfs to walk, and `sysctl -aN` prints the names without their values in a
/// few milliseconds. Failures fall back to an empty list, as everywhere else
/// here.
#[cfg(target_os = "macos")]
fn load_sysctl_keys() -> Vec<String> {
    run_command_lines("sysctl", &["-aN"], Path::new("/"))
        .map(dedup_sorted)
        .unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
fn collect_sysctl_keys(root: &Path, dir: &Path, values: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sysctl_keys(root, &path, values);
            continue;
        }

        if !path.is_file() {
            continue;
        }

        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let key = relative
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join(".");
        if !key.is_empty() {
            values.push(key);
        }
    }
}

/// Reads the currently loaded modules from `/proc/modules`, whose rows start
/// with the module name (`ext4 1052672 1 - Live 0x0000000000000000`).
fn load_loaded_kernel_module_names(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    dedup_sorted(
        contents
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_string)
            .collect(),
    )
}

fn load_kernel_module_names() -> Vec<String> {
    let release = run_command_lines("uname", &["-r"], Path::new("/"))
        .ok()
        .and_then(|lines| lines.into_iter().next());
    let root = release
        .map(|release| PathBuf::from("/lib/modules").join(release).join("kernel"))
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from("/lib/modules"));
    let mut values = Vec::new();
    collect_kernel_module_names(&root, &mut values);
    dedup_sorted(values)
}

fn collect_kernel_module_names(dir: &Path, values: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_kernel_module_names(&path, values);
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let module_name = file_name
            .strip_suffix(".ko")
            .or_else(|| file_name.strip_suffix(".ko.xz"))
            .or_else(|| file_name.strip_suffix(".ko.zst"));
        if let Some(module_name) = module_name {
            values.push(module_name.replace('-', "_"));
        }
    }
}

fn collect_wireguard_config_names_from_dirs<'a>(
    dirs: impl IntoIterator<Item = &'a Path>,
) -> Vec<String> {
    let mut values = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(name) = file_name.strip_suffix(".conf") else {
                continue;
            };
            if !name.is_empty() {
                values.push(name.to_string());
            }
        }
    }
    dedup_sorted(values)
}

fn format_task_description(source: &str, command: &str) -> String {
    let summary = format!("{source}: {command}");
    truncate_string(&summary, 80)
}

fn truncate_string(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let mut out: String = value.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests;
