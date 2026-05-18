use super::integrated::{CandidateType, EnhancedCandidate, matches_prefix};
use super::parser::{CompletionContext, ParsedCommandLine};
use super::shell_path::normalize_path_token;
use super::subprocess;
use crate::environment::Environment;
use anyhow::Result;
use dsh_builtin::{project_context, task};
use dsh_types::completion::is_known_dynamic_completion_provider;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime};
use tracing::warn;

mod dev;

const DYNAMIC_COMMAND_CACHE_TTL_MS: u64 = 1000;
const COMPLETION_COMMAND_TIMEOUT: Duration = Duration::from_millis(1500);
const EXTERNAL_COMPLETION_CACHE_LIMIT: usize = 128;
const JS_PROJECT_TASK_SOURCES: &[&str] = &["npm", "pnpm", "yarn", "bun"];
const DENO_PROJECT_TASK_SOURCES: &[&str] = &["deno"];
const TURBO_PROJECT_TASK_SOURCES: &[&str] = &["turbo"];
const NX_PROJECT_TASK_SOURCES: &[&str] = &["nx"];
const MISE_PROJECT_TASK_SOURCES: &[&str] = &["mise"];
const TASKFILE_PROJECT_TASK_SOURCES: &[&str] = &["taskfile"];
const JUST_PROJECT_TASK_SOURCES: &[&str] = &["just"];
const MAKE_PROJECT_TASK_SOURCES: &[&str] = &["make"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileMetadataSignature {
    exists: bool,
    modified: Option<SystemTime>,
    len: u64,
}

#[derive(Debug, Clone)]
struct TaskCacheEntry {
    signature: Vec<FileMetadataSignature>,
    tasks: Vec<task::TaskInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TaskCacheKey {
    project_root: PathBuf,
    sources: Vec<String>,
}

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

#[derive(Debug, Clone)]
struct ComposeCacheEntry {
    signature: FileMetadataSignature,
    services: Vec<String>,
}

#[derive(Debug, Clone)]
struct CommandValueCacheEntry {
    values: Vec<String>,
    cached_at: Instant,
    last_load_duration: Option<Duration>,
    last_error: Option<String>,
}

#[derive(Debug, Clone)]
struct CommandValueErrorEntry {
    recorded_at: Instant,
    last_load_duration: Duration,
    error: String,
}

#[derive(Debug, Clone)]
struct ExternalCompletionCacheEntry {
    candidates: Vec<EnhancedCandidate>,
    cached_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CargoMetadataValueKind {
    Package,
    Bin,
    Example,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemdUnitListKind {
    All,
    Running,
    Enabled,
    Disabled,
    UnitFiles,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DynamicCommandCacheKind {
    GitBranch,
    GitRemote,
    GitWorktree,
    KubectlContext,
    KubectlNamespace,
    CommandValue { command: String, value_kind: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DynamicCommandCacheKey {
    kind: DynamicCommandCacheKind,
    scope_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ExternalCompletionCacheKey {
    command_template: String,
    current_dir: PathBuf,
    input: String,
    cursor_pos: usize,
    command: String,
    current_token: String,
    subcommand_path: String,
}

#[derive(Debug, Default)]
struct ProjectDynamicCache {
    tasks: HashMap<TaskCacheKey, TaskCacheEntry>,
    compose_services: HashMap<PathBuf, ComposeCacheEntry>,
    commands: HashMap<DynamicCommandCacheKey, CommandValueCacheEntry>,
    command_errors: HashMap<DynamicCommandCacheKey, CommandValueErrorEntry>,
    command_pending: HashSet<DynamicCommandCacheKey>,
    external: HashMap<ExternalCompletionCacheKey, ExternalCompletionCacheEntry>,
    external_pending: HashSet<ExternalCompletionCacheKey>,
    external_pruned_total: usize,
}

pub(crate) struct DynamicCompletionProvider {
    environment: Arc<RwLock<Environment>>,
    cache: Arc<RwLock<ProjectDynamicCache>>,
}

#[derive(Debug, Default, Clone)]
struct DynamicCompletionDiagnostics {
    command_entries: usize,
    command_pending: usize,
    external_entries: usize,
    external_pending: usize,
    external_fish_entries: usize,
    external_pruned_total: usize,
    last_refresh: Option<Instant>,
    last_external: Option<String>,
    provider_lines: Vec<String>,
}

static DYNAMIC_COMPLETION_DIAGNOSTICS: LazyLock<RwLock<DynamicCompletionDiagnostics>> =
    LazyLock::new(|| RwLock::new(DynamicCompletionDiagnostics::default()));

pub(crate) fn diagnostics_lines() -> Vec<String> {
    let diagnostics = DYNAMIC_COMPLETION_DIAGNOSTICS.read().clone();
    let refresh = diagnostics
        .last_refresh
        .map(|instant| format!("{}ms-ago", instant.elapsed().as_millis()))
        .unwrap_or_else(|| "never".to_string());
    let external = diagnostics
        .last_external
        .unwrap_or_else(|| "none".to_string());

    let mut lines = vec![
        format!(
            "completion-cache dynamic-command entries={} pending={}",
            diagnostics.command_entries, diagnostics.command_pending
        ),
        format!(
            "completion-cache external entries={} pending={} fish={} limit={} pruned={} timeout={}ms last={}",
            diagnostics.external_entries,
            diagnostics.external_pending,
            diagnostics.external_fish_entries,
            EXTERNAL_COMPLETION_CACHE_LIMIT,
            diagnostics.external_pruned_total,
            COMPLETION_COMMAND_TIMEOUT.as_millis(),
            external
        ),
        format!("completion-cache last-refresh {refresh}"),
    ];
    lines.extend(diagnostics.provider_lines);
    lines
}

pub(crate) fn is_known_declared_dynamic_provider(provider: &str) -> bool {
    is_known_dynamic_completion_provider(provider)
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
        Self {
            environment,
            cache: Arc::new(RwLock::new(ProjectDynamicCache::default())),
        }
    }

    pub(crate) fn collect_task_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_task_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_declared_dynamic_candidates(
        &self,
        provider: &str,
        scope: Option<&str>,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let current_token = parsed_command_line.current_token.as_str();
        match provider {
            "git.branch" => {
                self.collect_git_branch_candidates(current_dir, current_token, cached_only)
            }
            "git.checkout_target" => {
                self.collect_git_checkout_target_candidates(current_dir, current_token, cached_only)
            }
            "git.changed_path" => {
                self.collect_git_changed_path_candidates(current_dir, current_token, cached_only)
            }
            "git.push_branch" => self.collect_git_push_branch_candidates(
                current_dir,
                selected_git_remote(parsed_command_line),
                current_token,
                cached_only,
            ),
            "git.remote" => {
                self.collect_git_remote_candidates(current_dir, current_token, cached_only)
            }
            "git.remote_branch" => self.collect_git_remote_branch_candidates(
                current_dir,
                selected_git_remote(parsed_command_line),
                current_token,
                cached_only,
            ),
            "git.revision" => {
                self.collect_git_revision_candidates(current_dir, current_token, cached_only)
            }
            "git.stash" => {
                self.collect_git_stash_candidates(current_dir, current_token, cached_only)
            }
            "git.tag" => self.collect_git_tag_candidates(current_dir, current_token, cached_only),
            "git.worktree" => {
                self.collect_git_worktree_candidates(current_dir, current_token, cached_only)
            }
            "docker.image" => {
                self.collect_docker_image_candidates(current_dir, current_token, cached_only)
            }
            "docker.container" => self.collect_docker_container_candidates(
                current_dir,
                current_token,
                scope != Some("running"),
                cached_only,
            ),
            "docker.network" => self.collect_container_object_candidates(
                "docker",
                "network",
                current_dir,
                current_token,
                "docker network",
                &["network", "ls", "--format", "{{.Name}}"],
                parse_non_empty_lines,
                cached_only,
            ),
            "docker.volume" => self.collect_container_object_candidates(
                "docker",
                "volume",
                current_dir,
                current_token,
                "docker volume",
                &["volume", "ls", "--format", "{{.Name}}"],
                parse_non_empty_lines,
                cached_only,
            ),
            "block.device" => {
                self.collect_block_device_candidates(current_dir, current_token, cached_only)
            }
            "block.label" => self.collect_blkid_attribute_candidates(
                current_dir,
                current_token,
                "LABEL",
                "block label",
                cached_only,
            ),
            "block.uuid" => self.collect_blkid_attribute_candidates(
                current_dir,
                current_token,
                "UUID",
                "block uuid",
                cached_only,
            ),
            "dbus.service" => {
                self.collect_dbus_service_candidates(current_dir, current_token, cached_only)
            }
            "docker.compose_service" => {
                let compose_file = selected_docker_compose_file(parsed_command_line, current_dir);
                if cached_only {
                    self.collect_compose_service_candidates_cached(
                        current_dir,
                        current_token,
                        compose_file.as_deref(),
                    )
                } else {
                    self.collect_compose_service_candidates(
                        current_dir,
                        current_token,
                        compose_file.as_deref(),
                    )
                }
            }
            "kubectl.context" => {
                self.collect_kubectl_context_candidates(current_dir, current_token, cached_only)
            }
            "kubectl.namespace" => {
                self.collect_kubectl_namespace_candidates(current_dir, current_token, cached_only)
            }
            "kubectl.resource_type" => self.collect_kubectl_resource_type_candidates(
                current_dir,
                current_token,
                cached_only,
            ),
            "kubectl.resource_name" => scope
                .or_else(|| {
                    split_kubectl_resource_name_token(current_token).map(|(resource, _)| resource)
                })
                .or_else(|| selected_kubectl_resource(parsed_command_line))
                .map(|resource| {
                    self.collect_kubectl_resource_name_candidates_for_token(
                        current_dir,
                        resource,
                        current_token,
                        selected_kubectl_namespace(parsed_command_line),
                        cached_only,
                    )
                })
                .unwrap_or_default(),
            "systemctl.unit" => {
                let kind = systemctl_unit_kind_for_context(parsed_command_line);
                self.collect_systemd_unit_candidates(
                    current_dir,
                    current_token,
                    kind,
                    selected_systemd_manager_scope(parsed_command_line),
                    "systemd unit",
                    cached_only,
                )
            }
            "systemctl.unit_file" => self.collect_systemd_unit_candidates(
                current_dir,
                current_token,
                SystemdUnitListKind::UnitFiles,
                selected_systemd_manager_scope(parsed_command_line),
                "systemd unit file",
                cached_only,
            ),
            "journalctl.boot" => {
                self.collect_journalctl_boot_candidates(current_dir, current_token, cached_only)
            }
            "firewalld.zone" => {
                self.collect_firewalld_zone_candidates(current_dir, current_token, cached_only)
            }
            "firewalld.service" => {
                self.collect_firewalld_service_candidates(current_dir, current_token, cached_only)
            }
            "firewalld.icmp_type" => {
                self.collect_firewalld_icmp_type_candidates(current_dir, current_token, cached_only)
            }
            "networkctl.link" => {
                self.collect_networkctl_link_candidates(current_dir, current_token, cached_only)
            }
            "ipset.set" => {
                self.collect_ipset_set_candidates(current_dir, current_token, cached_only)
            }
            "wireguard.interface" => {
                self.collect_wireguard_interface_candidates(current_dir, current_token, cached_only)
            }
            "wireguard.config" => {
                self.collect_wireguard_config_candidates(current_dir, current_token)
            }
            "cargo.package" => self.collect_cargo_metadata_candidates(
                current_dir,
                current_token,
                CargoMetadataValueKind::Package,
                "cargo package",
                cached_only,
            ),
            "cargo.bin" => self.collect_cargo_metadata_candidates(
                current_dir,
                current_token,
                CargoMetadataValueKind::Bin,
                "cargo binary target",
                cached_only,
            ),
            "cargo.example" => self.collect_cargo_metadata_candidates(
                current_dir,
                current_token,
                CargoMetadataValueKind::Example,
                "cargo example target",
                cached_only,
            ),
            "js.dependency" => self.collect_js_dependency_candidates(
                parsed_command_line,
                current_dir,
                parsed_command_line.command.as_str(),
                cached_only,
            ),
            "project.task" => {
                if let Some(config) = project_task_completion_config(scope, parsed_command_line) {
                    self.collect_project_task_candidates_for_sources_with_mode(
                        parsed_command_line,
                        current_dir,
                        config.sources,
                        cached_only,
                        config.candidate_text,
                    )
                } else if cached_only {
                    self.collect_task_candidates_with_mode(parsed_command_line, current_dir, true)
                } else {
                    self.collect_task_candidates(parsed_command_line, current_dir)
                }
            }
            "filesystem.type" => {
                self.collect_filesystem_type_candidates(current_token, cached_only)
            }
            "apt.installed_package" => self.collect_apt_installed_package_candidates(
                current_dir,
                current_token,
                parsed_command_line.command.as_str(),
                cached_only,
            ),
            "apk.installed_package" => self.collect_apk_installed_package_candidates(
                current_dir,
                current_token,
                cached_only,
            ),
            "dnf.installed_package" => self.collect_rpm_installed_package_candidates(
                current_dir,
                current_token,
                "dnf",
                cached_only,
            ),
            "rpm.installed_package" => self.collect_rpm_installed_package_candidates(
                current_dir,
                current_token,
                "rpm",
                cached_only,
            ),
            "zypper.installed_package" => self.collect_zypper_installed_package_candidates(
                current_dir,
                current_token,
                cached_only,
            ),
            "fstab.mountpoint" => {
                self.collect_fstab_mountpoint_candidates(current_token, cached_only)
            }
            "localectl.keymap" => {
                self.collect_localectl_keymap_candidates(current_dir, current_token, cached_only)
            }
            "localectl.locale" => {
                self.collect_localectl_locale_candidates(current_dir, current_token, cached_only)
            }
            "loginctl.seat" => {
                self.collect_loginctl_seat_candidates(current_dir, current_token, cached_only)
            }
            "loginctl.session" => {
                self.collect_loginctl_session_candidates(current_dir, current_token, cached_only)
            }
            "loop.device" => {
                self.collect_loop_device_candidates(current_dir, current_token, cached_only)
            }
            "sysctl.key" => self.collect_sysctl_key_candidates(current_token, cached_only),
            "ssh.host" => self.collect_ssh_host_candidates_with_mode(
                parsed_command_line,
                current_dir,
                parsed_command_line.command.as_str(),
                cached_only,
            ),
            "swap.device" => self.collect_swap_device_candidates(current_token, cached_only),
            "system.process_name" => self.collect_process_name_candidates_with_mode(
                parsed_command_line,
                "system",
                cached_only,
            ),
            "system.process_pid" => {
                self.collect_process_pid_candidates(parsed_command_line, cached_only)
            }
            "timedatectl.timezone" => self.collect_timedatectl_timezone_candidates(
                current_dir,
                current_token,
                cached_only,
            ),
            "tmux.session" => {
                self.collect_tmux_session_candidates(current_dir, current_token, cached_only)
            }
            "screen.session" => {
                self.collect_screen_session_candidates(current_dir, current_token, cached_only)
            }
            "nmcli.connection" => self.collect_nmcli_value_candidates(
                current_dir,
                current_token,
                NmcliCompletionSpec {
                    kind: "connection",
                    args: &["-t", "-f", "NAME", "connection", "show"],
                    description: "NetworkManager connection",
                    parser: parse_nmcli_first_field,
                },
                cached_only,
            ),
            "nmcli.device" => self.collect_nmcli_value_candidates(
                current_dir,
                current_token,
                NmcliCompletionSpec {
                    kind: "device",
                    args: &["-t", "-f", "DEVICE", "device"],
                    description: "NetworkManager device",
                    parser: parse_nmcli_first_field,
                },
                cached_only,
            ),
            "rustup.toolchain" => {
                self.collect_rustup_toolchain_candidates(current_dir, current_token, cached_only)
            }
            "pip.installed_package" => self.collect_pip_installed_package_candidates(
                current_dir,
                parsed_command_line.command.as_str(),
                current_token,
                cached_only,
            ),
            "pacman.package" => self.collect_pacman_package_candidates(
                current_dir,
                current_token,
                matches!(
                    parsed_command_line
                        .subcommand_path
                        .first()
                        .map(String::as_str)
                        .or_else(|| parsed_command_line.raw_args.first().map(String::as_str)),
                    Some("-S")
                ),
                cached_only,
            ),
            "mount.mountpoint" => {
                self.collect_mountpoint_candidates(current_dir, current_token, cached_only)
            }
            "kernel.module" => self.collect_kernel_module_candidates(current_token, cached_only),
            "aws.profile" => self.collect_aws_profile_candidates(current_token, cached_only),
            "gcloud.configuration" => {
                self.collect_gcloud_configuration_candidates(current_token, cached_only)
            }
            "gcloud.project" => self.collect_gcloud_project_candidates(current_token, cached_only),
            "python.project_dependency" => self.collect_python_project_dependency_candidates(
                current_dir,
                current_token,
                cached_only,
            ),
            "python.module" => {
                self.collect_python_module_candidates(current_dir, current_token, cached_only)
            }
            "node.bin" => self.collect_node_bin_candidates(current_dir, current_token, cached_only),
            "node.workspace" => {
                self.collect_node_workspace_candidates(current_dir, current_token, cached_only)
            }
            "go.package" => {
                self.collect_go_package_candidates(current_dir, current_token, cached_only)
            }
            "terraform.workspace" => {
                self.collect_terraform_workspace_candidates(current_dir, current_token, cached_only)
            }
            "podman.image" => self.collect_container_image_candidates(
                "podman",
                current_dir,
                current_token,
                cached_only,
            ),
            "podman.container" => self.collect_container_container_candidates(
                "podman",
                current_dir,
                current_token,
                scope != Some("running"),
                cached_only,
            ),
            "podman.network" => self.collect_container_object_candidates(
                "podman",
                "network",
                current_dir,
                current_token,
                "podman network",
                &["network", "ls", "--format", "{{.Name}}"],
                parse_non_empty_lines,
                cached_only,
            ),
            "podman.volume" => self.collect_container_object_candidates(
                "podman",
                "volume",
                current_dir,
                current_token,
                "podman volume",
                &["volume", "ls", "--format", "{{.Name}}"],
                parse_non_empty_lines,
                cached_only,
            ),
            _ => {
                warn!("Unknown dynamic completion provider: {provider}");
                Vec::new()
            }
        }
    }

    fn collect_task_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let current_token = parsed_command_line.current_token.as_str();
        let tasks = if cached_only {
            self.lookup_project_tasks(current_dir)
        } else {
            match self.load_project_tasks(current_dir) {
                Ok(tasks) => tasks,
                Err(e) => {
                    warn!("Failed to load task completions: {}", e);
                    return Vec::new();
                }
            }
        };

        tasks
            .into_iter()
            .filter(|task| matches_prefix(current_token, &task.name))
            .map(|task| EnhancedCandidate {
                text: task.name,
                description: Some(format_task_description(&task.source, &task.command)),
                candidate_type: CandidateType::Argument,
                priority: 90,
            })
            .collect()
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

    pub(crate) fn collect_git_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_git_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_git_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_git_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_git_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let Some(primary_subcommand) = parsed_command_line.subcommand_path.first() else {
            return Vec::new();
        };
        let current_token = parsed_command_line.current_token.as_str();
        let inferred_subcommand_arg_index =
            parsed_command_line.subcommand_path.len().saturating_sub(2);

        match &parsed_command_line.completion_context {
            CompletionContext::OptionValue { option_name, .. } => {
                if primary_subcommand == "restore"
                    && matches!(option_name.as_str(), "-s" | "--source")
                {
                    self.collect_git_revision_candidates(current_dir, current_token, cached_only)
                } else {
                    Vec::new()
                }
            }
            CompletionContext::Argument { arg_index, .. } => self.collect_git_argument_candidates(
                primary_subcommand,
                *arg_index,
                parsed_command_line,
                current_dir,
                current_token,
                cached_only,
            ),
            CompletionContext::SubCommand => self.collect_git_argument_candidates(
                primary_subcommand,
                inferred_subcommand_arg_index,
                parsed_command_line,
                current_dir,
                current_token,
                cached_only,
            ),
            _ => Vec::new(),
        }
    }

    fn collect_git_argument_candidates(
        &self,
        primary_subcommand: &str,
        arg_index: usize,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        match primary_subcommand {
            "checkout" => {
                self.collect_git_checkout_target_candidates(current_dir, current_token, cached_only)
            }
            "switch" | "merge" | "rebase" => {
                self.collect_git_branch_candidates(current_dir, current_token, cached_only)
            }
            "add" | "restore" => {
                self.collect_git_changed_path_candidates(current_dir, current_token, cached_only)
            }
            "push" => {
                if arg_index == 0 {
                    self.collect_git_remote_candidates(current_dir, current_token, cached_only)
                } else {
                    self.collect_git_push_branch_candidates(
                        current_dir,
                        selected_git_remote(parsed_command_line),
                        current_token,
                        cached_only,
                    )
                }
            }
            "pull" | "fetch" => {
                if arg_index == 0 {
                    self.collect_git_remote_candidates(current_dir, current_token, cached_only)
                } else {
                    self.collect_git_remote_branch_candidates(
                        current_dir,
                        selected_git_remote(parsed_command_line),
                        current_token,
                        cached_only,
                    )
                }
            }
            "log" | "diff" | "show" | "reset" => {
                self.collect_git_revision_candidates(current_dir, current_token, cached_only)
            }
            "branch" => self.collect_git_branch_candidates(current_dir, current_token, cached_only),
            "tag" => self.collect_git_tag_candidates(current_dir, current_token, cached_only),
            "stash" => {
                let secondary = parsed_command_line
                    .subcommand_path
                    .get(1)
                    .map(String::as_str)
                    .unwrap_or("");
                if matches!(secondary, "pop" | "apply" | "drop") {
                    self.collect_git_stash_candidates(current_dir, current_token, cached_only)
                } else {
                    Vec::new()
                }
            }
            "remote" => {
                let secondary = parsed_command_line
                    .subcommand_path
                    .get(1)
                    .map(String::as_str)
                    .unwrap_or("");
                match secondary {
                    "remove" | "rename" | "show" | "get-url" | "set-url" => {
                        self.collect_git_remote_candidates(current_dir, current_token, cached_only)
                    }
                    _ => Vec::new(),
                }
            }
            "worktree" => {
                let secondary = parsed_command_line
                    .subcommand_path
                    .get(1)
                    .map(String::as_str)
                    .unwrap_or("");
                match secondary {
                    "remove" | "move" | "lock" | "unlock" | "repair" => self
                        .collect_git_worktree_candidates(current_dir, current_token, cached_only),
                    "add" if arg_index > 0 => {
                        self.collect_git_branch_candidates(current_dir, current_token, cached_only)
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn collect_docker_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_docker_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_docker_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_docker_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_docker_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let Some(primary) = parsed_command_line
            .subcommand_path
            .first()
            .map(String::as_str)
        else {
            return Vec::new();
        };

        if primary != "compose" {
            return self.collect_docker_object_candidates(
                primary,
                parsed_command_line,
                current_dir,
                cached_only,
            );
        }

        let Some(command_name) = selected_docker_compose_command(parsed_command_line) else {
            return Vec::new();
        };

        match parsed_command_line.completion_context {
            CompletionContext::SubCommand | CompletionContext::Argument { .. } => {
                let service_commands = [
                    "build", "cp", "create", "down", "exec", "kill", "logs", "pause", "port", "ps",
                    "pull", "push", "restart", "rm", "run", "scale", "start", "stop", "top",
                    "unpause", "up", "wait",
                ];

                if service_commands.contains(&command_name) {
                    let current_token = parsed_command_line.current_token.as_str();
                    let compose_file =
                        selected_docker_compose_file(parsed_command_line, current_dir);
                    if cached_only {
                        self.collect_compose_service_candidates_cached(
                            current_dir,
                            current_token,
                            compose_file.as_deref(),
                        )
                    } else {
                        self.collect_compose_service_candidates(
                            current_dir,
                            current_token,
                            compose_file.as_deref(),
                        )
                    }
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn collect_docker_object_candidates(
        &self,
        subcommand: &str,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        match parsed_command_line.completion_context {
            CompletionContext::SubCommand | CompletionContext::Argument { .. } => {}
            _ => return Vec::new(),
        }

        match subcommand {
            "run" | "rmi" | "push" | "tag" => self.collect_docker_image_candidates(
                current_dir,
                parsed_command_line.current_token.as_str(),
                cached_only,
            ),
            "stop" | "restart" | "kill" | "logs" | "exec" | "attach" | "top" => self
                .collect_docker_container_candidates(
                    current_dir,
                    parsed_command_line.current_token.as_str(),
                    false,
                    cached_only,
                ),
            "rm" | "start" => self.collect_docker_container_candidates(
                current_dir,
                parsed_command_line.current_token.as_str(),
                true,
                cached_only,
            ),
            "inspect" => {
                let mut candidates = self.collect_docker_container_candidates(
                    current_dir,
                    parsed_command_line.current_token.as_str(),
                    true,
                    cached_only,
                );
                candidates.extend(self.collect_docker_image_candidates(
                    current_dir,
                    parsed_command_line.current_token.as_str(),
                    cached_only,
                ));
                candidates
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn collect_kubectl_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_kubectl_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_kubectl_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_kubectl_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_kubectl_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let current_token = parsed_command_line.current_token.as_str();
        match &parsed_command_line.completion_context {
            CompletionContext::OptionValue { option_name, .. } => match option_name.as_str() {
                "--context" => {
                    self.collect_kubectl_context_candidates(current_dir, current_token, cached_only)
                }
                "-n" | "--namespace" => self.collect_kubectl_namespace_candidates(
                    current_dir,
                    current_token,
                    cached_only,
                ),
                _ => Vec::new(),
            },
            CompletionContext::SubCommand | CompletionContext::Argument { .. } => {
                let words = kubectl_positional_words(parsed_command_line);
                if words.len() >= 2 && words[0] == "config" && words[1] == "use-context" {
                    self.collect_kubectl_context_candidates(current_dir, current_token, cached_only)
                } else if matches!(
                    words.first().copied(),
                    Some("get" | "describe" | "delete" | "edit" | "create" | "apply")
                ) {
                    let namespace = selected_kubectl_namespace(parsed_command_line);
                    if let Some((resource, _)) = split_kubectl_resource_name_token(current_token) {
                        self.collect_kubectl_resource_name_candidates_for_token(
                            current_dir,
                            resource,
                            current_token,
                            namespace,
                            cached_only,
                        )
                    } else if let Some(resource) = selected_kubectl_resource(parsed_command_line) {
                        if resource == current_token {
                            self.collect_kubectl_resource_type_candidates(
                                current_dir,
                                current_token,
                                cached_only,
                            )
                        } else {
                            self.collect_kubectl_resource_name_candidates_for_token(
                                current_dir,
                                resource,
                                current_token,
                                namespace,
                                cached_only,
                            )
                        }
                    } else {
                        self.collect_kubectl_resource_type_candidates(
                            current_dir,
                            current_token,
                            cached_only,
                        )
                    }
                } else if matches!(words.first().copied(), Some("logs" | "exec")) {
                    self.collect_kubectl_pod_candidates(current_dir, current_token, cached_only)
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn collect_cargo_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cargo_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_cargo_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cargo_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_cargo_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let CompletionContext::OptionValue { option_name, .. } =
            &parsed_command_line.completion_context
        else {
            return Vec::new();
        };

        let (kind, description) = match option_name.as_str() {
            "-p" | "--package" => (CargoMetadataValueKind::Package, "cargo package"),
            "--bin" => (CargoMetadataValueKind::Bin, "cargo binary target"),
            "--example" => (CargoMetadataValueKind::Example, "cargo example target"),
            _ => return Vec::new(),
        };

        self.collect_cargo_metadata_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            kind,
            description,
            cached_only,
        )
    }

    pub(crate) fn collect_systemctl_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_systemctl_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_systemctl_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_systemctl_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_systemctl_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }

        let Some(subcommand) = parsed_command_line
            .subcommand_path
            .first()
            .map(String::as_str)
        else {
            return Vec::new();
        };
        let kind = match systemctl_unit_kind_for_subcommand(subcommand) {
            Some(kind) => kind,
            _ => return Vec::new(),
        };

        self.collect_systemd_unit_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            kind,
            selected_systemd_manager_scope(parsed_command_line),
            "systemd unit",
            cached_only,
        )
    }

    pub(crate) fn collect_journalctl_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_journalctl_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_journalctl_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_journalctl_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_journalctl_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
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
            SystemdUnitListKind::All,
            selected_systemd_manager_scope(parsed_command_line),
            "systemd unit",
            cached_only,
        )
    }

    pub(crate) fn collect_ssh_host_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        command_name: &str,
    ) -> Vec<EnhancedCandidate> {
        self.collect_ssh_host_candidates_with_mode(
            parsed_command_line,
            current_dir,
            command_name,
            false,
        )
    }

    pub(crate) fn collect_ssh_host_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        command_name: &str,
    ) -> Vec<EnhancedCandidate> {
        self.collect_ssh_host_candidates_with_mode(
            parsed_command_line,
            current_dir,
            command_name,
            true,
        )
    }

    fn collect_ssh_host_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        _current_dir: &Path,
        command_name: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }
        let current_token = parsed_command_line.current_token.as_str();
        if current_token.contains(':') {
            return Vec::new();
        }

        let scope = ssh_config_scope();
        let loader = move || Ok(load_ssh_hosts());
        let values = self.load_or_lookup_command_values(
            command_name,
            "ssh-host",
            scope,
            cached_only,
            loader,
        );
        let user_prefix = current_token
            .rsplit_once('@')
            .map(|(user, _)| user.to_string());
        let host_token = current_token
            .rsplit_once('@')
            .map_or(current_token, |(_, host)| host);

        values
            .into_iter()
            .filter(|host| matches_prefix(host_token, host))
            .map(|host| {
                let text =
                    format_ssh_host_candidate_text(command_name, user_prefix.as_deref(), host);
                EnhancedCandidate {
                    text,
                    description: Some("ssh host".to_string()),
                    candidate_type: CandidateType::Argument,
                    priority: 130,
                }
            })
            .collect()
    }

    pub(crate) fn collect_tmux_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_tmux_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_tmux_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_tmux_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_tmux_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
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

        self.collect_tmux_session_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            cached_only,
        )
    }

    pub(crate) fn collect_screen_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_screen_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_screen_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_screen_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_screen_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
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

        self.collect_screen_session_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            cached_only,
        )
    }

    pub(crate) fn collect_process_name_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        command_name: &str,
    ) -> Vec<EnhancedCandidate> {
        self.collect_process_name_candidates_with_mode(parsed_command_line, command_name, false)
    }

    pub(crate) fn collect_process_name_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        command_name: &str,
    ) -> Vec<EnhancedCandidate> {
        self.collect_process_name_candidates_with_mode(parsed_command_line, command_name, true)
    }

    fn collect_process_name_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        command_name: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
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
    ) -> Vec<EnhancedCandidate> {
        self.collect_pip_candidates_with_mode(parsed_command_line, current_dir, command_name, false)
    }

    pub(crate) fn collect_pip_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        command_name: &str,
    ) -> Vec<EnhancedCandidate> {
        self.collect_pip_candidates_with_mode(parsed_command_line, current_dir, command_name, true)
    }

    fn collect_pip_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        command_name: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
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
    ) -> Vec<EnhancedCandidate> {
        self.collect_rustup_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_rustup_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_rustup_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_rustup_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
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
        self.collect_rustup_toolchain_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            cached_only,
        )
    }

    pub(crate) fn collect_gh_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_gh_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_gh_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_gh_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_gh_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
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
            project_context::find_project_root(&current_dir),
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
    ) -> Vec<EnhancedCandidate> {
        self.collect_nmcli_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_nmcli_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_nmcli_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_nmcli_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
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
    ) -> Vec<EnhancedCandidate> {
        self.collect_pacman_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_pacman_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_pacman_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_pacman_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let subcommand = parsed_command_line
            .subcommand_path
            .first()
            .map(String::as_str)
            .or_else(|| parsed_command_line.raw_args.first().map(String::as_str));
        let sync = match subcommand {
            Some("-S") => true,
            Some("-R") => false,
            _ => return Vec::new(),
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
    ) -> Vec<EnhancedCandidate> {
        self.collect_mount_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_mount_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_mount_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_mount_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }

        let current_token = parsed_command_line.current_token.as_str();
        let mut candidates =
            self.collect_block_device_candidates(current_dir, current_token, cached_only);
        candidates.extend(self.collect_fstab_mountpoint_candidates(current_token, cached_only));
        candidates
    }

    pub(crate) fn collect_umount_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_umount_candidates_with_mode(parsed_command_line, current_dir, false)
    }

    pub(crate) fn collect_umount_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.collect_umount_candidates_with_mode(parsed_command_line, current_dir, true)
    }

    fn collect_umount_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
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
    ) -> Vec<EnhancedCandidate> {
        self.collect_modprobe_candidates_with_mode(parsed_command_line, false)
    }

    pub(crate) fn collect_modprobe_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        self.collect_modprobe_candidates_with_mode(parsed_command_line, true)
    }

    fn collect_modprobe_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }
        self.collect_kernel_module_candidates(
            parsed_command_line.current_token.as_str(),
            cached_only,
        )
    }

    fn collect_tmux_session_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("tmux");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "tmux",
            "session",
            canonicalize_path(&current_dir),
            current_token,
            "tmux session",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                run_command_lines(
                    &command_path,
                    &["list-sessions", "-F", "#{session_name}"],
                    &current_dir,
                )
            },
        )
    }

    fn collect_screen_session_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("screen");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "screen",
            "session",
            canonicalize_path(&current_dir),
            current_token,
            "screen session",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_screen_sessions(&run_command_lines(
                    &command_path,
                    &["-ls"],
                    &current_dir,
                )?))
            },
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

    fn collect_rustup_toolchain_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("rustup");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "rustup",
            "toolchain",
            canonicalize_path(&current_dir),
            current_token,
            "rustup toolchain",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_first_fields(&run_command_lines(
                    &command_path,
                    &["toolchain", "list"],
                    &current_dir,
                )?))
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

    fn collect_kernel_module_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
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

    fn collect_filesystem_type_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cached_value_candidates(
            "filesystem",
            "type",
            PathBuf::from("/proc/filesystems"),
            current_token,
            "filesystem type",
            cached_only,
            || Ok(load_filesystem_types()),
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

    fn collect_block_device_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("lsblk");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "block",
            "device",
            PathBuf::from("/sys/block"),
            current_token,
            "block device",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_lsblk_devices(&run_command_lines(
                    &command_path,
                    &["-rno", "NAME,TYPE"],
                    &current_dir,
                )?))
            },
        )
    }

    fn collect_dbus_service_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "busctl",
            "service",
            PathBuf::from("/run/dbus"),
            "busctl",
            &["list"],
            current_dir,
            current_token,
            "D-Bus service",
            cached_only,
            parse_busctl_services,
        )
    }

    fn collect_fstab_mountpoint_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cached_value_candidates(
            "fstab",
            "mountpoint",
            PathBuf::from("/etc/fstab"),
            current_token,
            "fstab mount point",
            cached_only,
            || Ok(load_fstab_mountpoints()),
        )
    }

    fn collect_localectl_locale_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "localectl",
            "locale",
            PathBuf::from("/usr/lib/locale"),
            "localectl",
            &["list-locales"],
            current_dir,
            current_token,
            "locale",
            cached_only,
            parse_package_lines,
        )
    }

    fn collect_localectl_keymap_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "localectl",
            "keymap",
            PathBuf::from("/usr/share/kbd/keymaps"),
            "localectl",
            &["list-keymaps"],
            current_dir,
            current_token,
            "keymap",
            cached_only,
            parse_package_lines,
        )
    }

    fn collect_loginctl_session_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "loginctl",
            "session",
            PathBuf::from("/run/systemd/sessions"),
            "loginctl",
            &["list-sessions", "--no-legend"],
            current_dir,
            current_token,
            "login session",
            cached_only,
            parse_loginctl_sessions,
        )
    }

    fn collect_loginctl_seat_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "loginctl",
            "seat",
            PathBuf::from("/run/systemd/seats"),
            "loginctl",
            &["list-seats", "--no-legend"],
            current_dir,
            current_token,
            "login seat",
            cached_only,
            parse_loginctl_seats,
        )
    }

    fn collect_loop_device_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "losetup",
            "loop-device",
            PathBuf::from("/sys/block"),
            "losetup",
            &["--list", "--noheadings", "--output", "NAME"],
            current_dir,
            current_token,
            "loop device",
            cached_only,
            parse_losetup_devices,
        )
    }

    fn collect_swap_device_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cached_value_candidates(
            "swap",
            "device",
            PathBuf::from("/proc/swaps"),
            current_token,
            "swap device",
            cached_only,
            || Ok(load_swap_devices()),
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

    fn collect_timedatectl_timezone_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "timedatectl",
            "timezone",
            PathBuf::from("/usr/share/zoneinfo"),
            "timedatectl",
            &["list-timezones"],
            current_dir,
            current_token,
            "time zone",
            cached_only,
            parse_package_lines,
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

    fn collect_rpm_installed_package_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        command_name: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("rpm");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            command_name,
            "installed-package",
            PathBuf::from("/var/lib/rpm"),
            current_token,
            "installed rpm package",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_package_lines(&run_command_lines(
                    &command_path,
                    &["-qa", "--qf", "%{NAME}\\n"],
                    &current_dir,
                )?))
            },
        )
    }

    fn collect_apk_installed_package_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "apk",
            "installed-package",
            PathBuf::from("/lib/apk/db/installed"),
            "apk",
            &["info"],
            current_dir,
            current_token,
            "installed apk package",
            cached_only,
            parse_package_lines,
        )
    }

    fn collect_zypper_installed_package_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "zypper",
            "installed-package",
            PathBuf::from("/var/lib/rpm"),
            "rpm",
            &["-qa", "--qf", "%{NAME}\\n"],
            current_dir,
            current_token,
            "installed rpm package",
            cached_only,
            parse_package_lines,
        )
    }

    fn collect_journalctl_boot_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "journalctl",
            "boot",
            PathBuf::from("/var/log/journal"),
            "journalctl",
            &["--list-boots", "--no-pager"],
            current_dir,
            current_token,
            "journal boot",
            cached_only,
            parse_journalctl_boots,
        )
    }

    fn collect_firewalld_zone_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "firewall-cmd",
            "zone",
            PathBuf::from("/etc/firewalld/zones"),
            "firewall-cmd",
            &["--get-zones"],
            current_dir,
            current_token,
            "firewalld zone",
            cached_only,
            parse_whitespace_values,
        )
    }

    fn collect_firewalld_service_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "firewall-cmd",
            "service",
            PathBuf::from("/usr/lib/firewalld/services"),
            "firewall-cmd",
            &["--get-services"],
            current_dir,
            current_token,
            "firewalld service",
            cached_only,
            parse_whitespace_values,
        )
    }

    fn collect_firewalld_icmp_type_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "firewall-cmd",
            "icmp-type",
            PathBuf::from("/usr/lib/firewalld/icmptypes"),
            "firewall-cmd",
            &["--get-icmptypes"],
            current_dir,
            current_token,
            "firewalld ICMP type",
            cached_only,
            parse_whitespace_values,
        )
    }

    fn collect_networkctl_link_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "networkctl",
            "link",
            PathBuf::from("/sys/class/net"),
            "networkctl",
            &["list", "--all", "--no-legend", "--no-pager"],
            current_dir,
            current_token,
            "network link",
            cached_only,
            parse_networkctl_links,
        )
    }

    fn collect_ipset_set_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "ipset",
            "set",
            PathBuf::from("/etc/ipset.conf"),
            "ipset",
            &["list", "-n"],
            current_dir,
            current_token,
            "ipset set",
            cached_only,
            parse_package_lines,
        )
    }

    fn collect_wireguard_interface_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_local_command_value_candidates(
            "wg",
            "interface",
            PathBuf::from("/etc/wireguard"),
            "wg",
            &["show", "interfaces"],
            current_dir,
            current_token,
            "WireGuard interface",
            cached_only,
            parse_whitespace_values,
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
    ) -> Vec<EnhancedCandidate> {
        self.collect_tcpdump_candidates_with_mode(parsed_command_line, false)
    }

    pub(crate) fn collect_tcpdump_candidates_cached(
        &self,
        parsed_command_line: &ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        self.collect_tcpdump_candidates_with_mode(parsed_command_line, true)
    }

    fn collect_tcpdump_candidates_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
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

    pub(crate) fn collect_project_task_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        sources: &[&str],
    ) -> Vec<EnhancedCandidate> {
        self.collect_project_task_candidates_for_sources_with_mode(
            parsed_command_line,
            current_dir,
            sources,
            false,
            ProjectTaskCandidateText::Name,
        )
    }

    fn collect_project_task_candidates_for_sources_with_mode(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        sources: &[&str],
        cached_only: bool,
        candidate_text: ProjectTaskCandidateText,
    ) -> Vec<EnhancedCandidate> {
        let current_token = parsed_command_line.current_token.as_str();
        match parsed_command_line.completion_context {
            CompletionContext::Command
            | CompletionContext::SubCommand
            | CompletionContext::Argument { .. } => {}
            _ => return Vec::new(),
        }

        let tasks = if cached_only {
            self.lookup_project_tasks_for_sources(current_dir, sources)
        } else {
            match self.load_project_tasks_for_sources(current_dir, sources) {
                Ok(tasks) => tasks,
                Err(err) => {
                    warn!("Failed to load project task completions: {}", err);
                    return Vec::new();
                }
            }
        };

        tasks
            .into_iter()
            .filter(|task| sources.contains(&task.source.as_str()))
            .filter_map(|task| {
                let text = project_task_candidate_text(&task, candidate_text);
                matches_prefix(current_token, &text).then_some((task, text))
            })
            .map(|(task, text)| EnhancedCandidate {
                text,
                description: Some(format_task_description(&task.source, &task.command)),
                candidate_type: CandidateType::Argument,
                priority: 125,
            })
            .collect()
    }

    pub(crate) fn collect_external_candidates(
        &self,
        current_dir: &Path,
        input: &str,
        cursor_pos: usize,
        parsed_command_line: &ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        let Some(command_template) = self
            .environment
            .read()
            .get_var("DSH_EXTERNAL_COMPLETER")
            .filter(|value| !value.trim().is_empty())
        else {
            return Vec::new();
        };

        let subcommand_path = parsed_command_line.subcommand_path.join(" ");
        let key = ExternalCompletionCacheKey {
            command_template: command_template.clone(),
            current_dir: canonicalize_path(current_dir),
            input: input.to_string(),
            cursor_pos,
            command: parsed_command_line.command.clone(),
            current_token: parsed_command_line.current_token.clone(),
            subcommand_path,
        };

        let loader_key = key.clone();
        match self
            .load_external_candidates(key, move || run_external_completer_for_key(&loader_key))
        {
            Ok(candidates) => candidates,
            Err(err) => {
                warn!("External completer failed: {}", err);
                Vec::new()
            }
        }
    }

    fn collect_git_branch_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitBranch,
            scope_dir,
            current_token,
            "git branch",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };

                    run_command_lines(
                        &command_path,
                        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
                        &current_dir,
                    )
                }
            },
        )
    }

    fn collect_git_remote_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitRemote,
            scope_dir,
            current_token,
            "git remote",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };

                    run_command_lines(&command_path, &["remote"], &current_dir)
                }
            },
        )
    }

    fn collect_git_worktree_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitWorktree,
            scope_dir,
            current_token,
            "git worktree",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };

                    Ok(run_command_lines(
                        &command_path,
                        &["worktree", "list", "--porcelain"],
                        &current_dir,
                    )?
                    .into_iter()
                    .filter_map(|line| line.strip_prefix("worktree ").map(str::to_string))
                    .collect())
                }
            },
        )
    }

    fn collect_git_checkout_target_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_value_candidates(
            "git",
            "checkout-target",
            scope_dir,
            current_token,
            "git branch",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    let mut values = run_command_lines(
                        &command_path,
                        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
                        &current_dir,
                    )?;
                    values.extend(parse_git_remote_branches(
                        &run_command_lines(
                            &command_path,
                            &["for-each-ref", "--format=%(refname:short)", "refs/remotes"],
                            &current_dir,
                        )?,
                        None,
                    ));
                    Ok(dedup_sorted(values))
                }
            },
        )
    }

    fn collect_git_remote_branch_candidates(
        &self,
        current_dir: &Path,
        remote: Option<&str>,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        let remote = remote.map(str::to_string);
        let value_kind = format!(
            "remote-branch:{}",
            remote
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("*")
        );
        self.collect_cached_value_candidates(
            "git",
            &value_kind,
            scope_dir,
            current_token,
            "git remote branch",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    Ok(parse_git_remote_branches(
                        &run_command_lines(
                            &command_path,
                            &["for-each-ref", "--format=%(refname:short)", "refs/remotes"],
                            &current_dir,
                        )?,
                        remote.as_deref(),
                    ))
                }
            },
        )
    }

    fn collect_git_push_branch_candidates(
        &self,
        current_dir: &Path,
        remote: Option<&str>,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        let remote = remote.map(str::to_string);
        let value_kind = format!(
            "push-branch:{}",
            remote
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("*")
        );
        self.collect_cached_value_candidates(
            "git",
            &value_kind,
            scope_dir,
            current_token,
            "git branch",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    let mut values = run_command_lines(
                        &command_path,
                        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
                        &current_dir,
                    )?;
                    values.extend(parse_git_remote_branches(
                        &run_command_lines(
                            &command_path,
                            &["for-each-ref", "--format=%(refname:short)", "refs/remotes"],
                            &current_dir,
                        )?,
                        remote.as_deref(),
                    ));
                    Ok(dedup_sorted(values))
                }
            },
        )
    }

    fn collect_git_revision_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_value_candidates(
            "git",
            "revision",
            scope_dir,
            current_token,
            "git revision",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    let mut values = run_command_lines(
                        &command_path,
                        &[
                            "for-each-ref",
                            "--format=%(refname:short)",
                            "refs/heads",
                            "refs/tags",
                        ],
                        &current_dir,
                    )?;
                    values.extend([
                        "HEAD".to_string(),
                        "FETCH_HEAD".to_string(),
                        "ORIG_HEAD".to_string(),
                    ]);
                    Ok(dedup_sorted(values))
                }
            },
        )
    }

    fn collect_git_tag_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_value_candidates(
            "git",
            "tag",
            scope_dir,
            current_token,
            "git tag",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    run_command_lines(&command_path, &["tag"], &current_dir)
                }
            },
        )
    }

    fn collect_git_stash_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_value_candidates(
            "git",
            "stash",
            scope_dir,
            current_token,
            "git stash",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    Ok(parse_git_stash_refs(&run_command_lines(
                        &command_path,
                        &["stash", "list"],
                        &current_dir,
                    )?))
                }
            },
        )
    }

    fn collect_git_changed_path_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        let command_path = self.resolve_command_path("git");
        self.collect_cached_value_candidates(
            "git",
            "changed-path",
            scope_dir,
            current_token,
            "git changed path",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    Ok(parse_git_status_porcelain_paths(&run_command_stdout(
                        &command_path,
                        &["status", "--porcelain", "-z"],
                        &current_dir,
                    )?))
                }
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_local_command_value_candidates(
        &self,
        command_name: &str,
        value_kind: &str,
        scope_dir: PathBuf,
        executable: &str,
        args: &'static [&'static str],
        current_dir: &Path,
        current_token: &str,
        description: &str,
        cached_only: bool,
        parser: fn(&[String]) -> Vec<String>,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path(executable);
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            command_name,
            value_kind,
            scope_dir,
            current_token,
            description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parser(&run_command_lines(
                    &command_path,
                    args,
                    &current_dir,
                )?))
            },
        )
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
        self.load_or_lookup_command_values(command_name, value_kind, scope_dir, cached_only, loader)
            .into_iter()
            .filter(|value| matches_prefix(current_token, value))
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
            self.load_command_values(kind, scope_dir, loader)
        }
    }

    fn collect_docker_image_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_container_image_candidates("docker", current_dir, current_token, cached_only)
    }

    fn collect_docker_container_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        include_stopped: bool,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_container_container_candidates(
            "docker",
            current_dir,
            current_token,
            include_stopped,
            cached_only,
        )
    }

    fn collect_container_image_candidates(
        &self,
        executable: &'static str,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_container_object_candidates(
            executable,
            "image",
            current_dir,
            current_token,
            &format!("{executable} image"),
            &["images", "--format", "{{.Repository}}:{{.Tag}}"],
            parse_container_images,
            cached_only,
        )
    }

    fn collect_container_container_candidates(
        &self,
        executable: &'static str,
        current_dir: &Path,
        current_token: &str,
        include_stopped: bool,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let current_dir = current_dir.to_path_buf();
        let value_kind = if include_stopped {
            "container-all"
        } else {
            "container-running"
        };
        let args: &'static [&'static str] = if include_stopped {
            &["ps", "-a", "--format", "{{.Names}}"]
        } else {
            &["ps", "--format", "{{.Names}}"]
        };
        self.collect_container_object_candidates(
            executable,
            value_kind,
            &current_dir,
            current_token,
            &format!("{executable} container"),
            args,
            parse_non_empty_lines,
            cached_only,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_container_object_candidates(
        &self,
        executable: &'static str,
        value_kind: &'static str,
        current_dir: &Path,
        current_token: &str,
        description: &str,
        args: &'static [&'static str],
        parser: fn(&[String]) -> Vec<String>,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path(executable);
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            executable,
            value_kind,
            canonicalize_path(&current_dir),
            current_token,
            description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parser(&run_command_lines(
                    &command_path,
                    args,
                    &current_dir,
                )?))
            },
        )
    }

    fn collect_compose_service_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        compose_file_override: Option<&Path>,
    ) -> Vec<EnhancedCandidate> {
        match self.load_compose_services(current_dir, compose_file_override) {
            Ok(Some((compose_file, services))) => services
                .into_iter()
                .filter(|service| matches_prefix(current_token, service))
                .map(|service| EnhancedCandidate {
                    text: service,
                    description: Some(format!("compose service ({})", compose_file.display())),
                    candidate_type: CandidateType::Argument,
                    priority: 125,
                })
                .collect(),
            Ok(None) => Vec::new(),
            Err(err) => {
                warn!(
                    "Failed to load compose services from {:?}: {}",
                    current_dir, err
                );
                Vec::new()
            }
        }
    }

    fn collect_compose_service_candidates_cached(
        &self,
        current_dir: &Path,
        current_token: &str,
        compose_file_override: Option<&Path>,
    ) -> Vec<EnhancedCandidate> {
        let compose_file = if let Some(path) = compose_file_override {
            canonicalize_path(path)
        } else {
            let Some(compose_file) = find_compose_file(current_dir) else {
                return Vec::new();
            };
            canonicalize_path(&compose_file)
        };
        let signature = file_metadata_signature(&compose_file);
        self.lookup_compose_cache(&compose_file, &signature)
            .unwrap_or_default()
            .into_iter()
            .filter(|service| matches_prefix(current_token, service))
            .map(|service| EnhancedCandidate {
                text: service,
                description: Some(format!("compose service ({})", compose_file.display())),
                candidate_type: CandidateType::Argument,
                priority: 125,
            })
            .collect()
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
        let scope_dir = project_context::find_project_root(&current_dir);
        let value_kind = match kind {
            CargoMetadataValueKind::Package => "package",
            CargoMetadataValueKind::Bin => "bin",
            CargoMetadataValueKind::Example => "example",
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
        kind: SystemdUnitListKind,
        manager_scope: Option<SystemdManagerScope>,
        description: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
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
                Ok(parse_first_fields(&run_command_lines(
                    &command_path,
                    &args,
                    &current_dir,
                )?))
            },
        )
    }

    fn collect_kubectl_resource_type_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("kubectl");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "kubectl",
            "resource-type",
            canonicalize_path(&current_dir),
            current_token,
            "kubectl resource",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(run_command_lines(
                    &command_path,
                    &["api-resources", "--namespaced=true", "-o", "name"],
                    &current_dir,
                )?
                .into_iter()
                .filter_map(|resource| resource.split('/').next().map(str::to_string))
                .collect())
            },
        )
    }

    fn collect_kubectl_resource_name_candidates(
        &self,
        current_dir: &Path,
        resource: &str,
        current_token: &str,
        namespace: Option<&str>,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("kubectl");
        let current_dir = current_dir.to_path_buf();
        let resource = resource.to_string();
        let namespace = namespace.map(str::to_string);
        let value_kind = namespace
            .as_deref()
            .map(|namespace| format!("resource-name:{namespace}:{resource}"))
            .unwrap_or_else(|| format!("resource-name:{resource}"));
        self.collect_cached_value_candidates(
            "kubectl",
            &value_kind,
            canonicalize_path(&current_dir),
            current_token,
            "kubectl resource name",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let mut args = vec!["get"];
                if let Some(namespace) = namespace.as_deref() {
                    args.push("-n");
                    args.push(namespace);
                }
                args.push(&resource);
                args.push("-o");
                args.push("jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}");
                run_command_lines(&command_path, &args, &current_dir)
            },
        )
    }

    fn collect_kubectl_resource_name_candidates_for_token(
        &self,
        current_dir: &Path,
        resource: &str,
        current_token: &str,
        namespace: Option<&str>,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if let Some((token_resource, name_prefix)) =
            split_kubectl_resource_name_token(current_token)
        {
            return self
                .collect_kubectl_resource_name_candidates(
                    current_dir,
                    token_resource,
                    name_prefix,
                    namespace,
                    cached_only,
                )
                .into_iter()
                .map(|mut candidate| {
                    candidate.text = format!("{token_resource}/{}", candidate.text);
                    candidate
                })
                .collect();
        }

        self.collect_kubectl_resource_name_candidates(
            current_dir,
            resource,
            current_token,
            namespace,
            cached_only,
        )
    }

    fn collect_kubectl_pod_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("kubectl");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "kubectl",
            "pod",
            canonicalize_path(&current_dir),
            current_token,
            "kubectl pod",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                run_command_lines(
                    &command_path,
                    &[
                        "get",
                        "pods",
                        "-o",
                        "jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}",
                    ],
                    &current_dir,
                )
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

        let project_root = project_context::find_project_root(current_dir);
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

    fn collect_kubectl_context_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("kubectl");
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::KubectlContext,
            canonicalize_path(current_dir),
            current_token,
            "kubectl context",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };

                    run_command_lines(
                        &command_path,
                        &["config", "get-contexts", "-o", "name"],
                        &current_dir,
                    )
                }
            },
        )
    }

    fn collect_kubectl_namespace_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("kubectl");
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::KubectlNamespace,
            canonicalize_path(current_dir),
            current_token,
            "kubectl namespace",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };

                    run_command_lines(
                        &command_path,
                        &[
                            "get",
                            "namespaces",
                            "-o",
                            "jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}",
                        ],
                        &current_dir,
                    )
                }
            },
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

        values
            .into_iter()
            .filter(|value| matches_prefix(current_token, value))
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
        let project_root = project_context::find_project_root(current_dir);
        let cache_key = TaskCacheKey {
            project_root: project_root.clone(),
            sources: Vec::new(),
        };
        let signature = task_completion_signature(&project_root);

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
        let project_root = project_context::find_project_root(current_dir);
        let cache_key = TaskCacheKey {
            project_root: project_root.clone(),
            sources: Vec::new(),
        };
        let signature = task_completion_signature(&project_root);
        self.lookup_task_cache(&cache_key, &signature)
            .unwrap_or_default()
    }

    fn load_project_tasks_for_sources(
        &self,
        current_dir: &Path,
        sources: &[&str],
    ) -> Result<Vec<task::TaskInfo>> {
        let project_root = project_context::find_project_root(current_dir);
        let cache_key = TaskCacheKey {
            project_root: project_root.clone(),
            sources: normalized_task_sources(sources),
        };
        let signature = task_completion_signature(&project_root);

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
        let project_root = project_context::find_project_root(current_dir);
        let cache_key = TaskCacheKey {
            project_root: project_root.clone(),
            sources: normalized_task_sources(sources),
        };
        let signature = task_completion_signature(&project_root);
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
        let cache_key = DynamicCommandCacheKey { kind, scope_dir };
        let ttl = Duration::from_millis(DYNAMIC_COMMAND_CACHE_TTL_MS);

        {
            let mut cache = self.cache.write();
            if let Some(entry) = cache.commands.get(&cache_key) {
                let values = entry.values.clone();
                let start_refresh = entry.cached_at.elapsed() >= ttl
                    && cache.command_pending.insert(cache_key.clone());
                update_diagnostics_from_cache(&cache, None);
                drop(cache);
                if start_refresh {
                    spawn_command_refresh(self.cache.clone(), cache_key, loader);
                }
                return values;
            }

            if !cache.command_pending.insert(cache_key.clone()) {
                update_diagnostics_from_cache(&cache, None);
                return Vec::new();
            }
            update_diagnostics_from_cache(&cache, None);
        }

        let load_started = Instant::now();
        let result = loader();
        let load_duration = load_started.elapsed();
        let mut cache = self.cache.write();
        cache.command_pending.remove(&cache_key);
        match result {
            Ok(values) => {
                cache.command_errors.remove(&cache_key);
                cache.commands.insert(
                    cache_key,
                    CommandValueCacheEntry {
                        values: values.clone(),
                        cached_at: Instant::now(),
                        last_load_duration: Some(load_duration),
                        last_error: None,
                    },
                );
                update_diagnostics_from_cache(&cache, None);
                values
            }
            Err(err) => {
                warn!("Dynamic command completion initial load failed: {}", err);
                cache.command_errors.insert(
                    cache_key,
                    CommandValueErrorEntry {
                        recorded_at: Instant::now(),
                        last_load_duration: load_duration,
                        error: err.to_string(),
                    },
                );
                update_diagnostics_from_cache(&cache, None);
                Vec::new()
            }
        }
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
                update_diagnostics_from_cache(&cache, None);
                drop(cache);
                if start_refresh {
                    spawn_external_refresh(self.cache.clone(), cache_key, loader);
                }
                return Ok(candidates);
            }

            if !cache.external_pending.insert(cache_key.clone()) {
                update_diagnostics_from_cache(&cache, None);
                return Ok(Vec::new());
            }
            update_diagnostics_from_cache(&cache, Some("external initial-load".to_string()));
        }

        let result = loader();
        let mut cache = self.cache.write();
        cache.external_pending.remove(&cache_key);
        match result {
            Ok(candidates) => {
                if candidates.is_empty() {
                    update_diagnostics_from_cache(&cache, Some("external empty".to_string()));
                } else {
                    insert_external_cache_entry(
                        &mut cache,
                        cache_key,
                        ExternalCompletionCacheEntry {
                            candidates: candidates.clone(),
                            cached_at: Instant::now(),
                        },
                    );
                    update_diagnostics_from_cache(&cache, Some("external ok".to_string()));
                }
                Ok(candidates)
            }
            Err(err) => {
                update_diagnostics_from_cache(&cache, Some(format!("external error: {err}")));
                Err(err)
            }
        }
    }

    fn resolve_command_path(&self, command_name: &str) -> Option<String> {
        self.environment.read().lookup(command_name)
    }
}

fn canonicalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn spawn_command_refresh<F>(
    cache: Arc<RwLock<ProjectDynamicCache>>,
    cache_key: DynamicCommandCacheKey,
    loader: F,
) where
    F: FnOnce() -> Result<Vec<String>> + Send + 'static,
{
    std::thread::spawn(move || {
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
                update_diagnostics_from_cache(&cache, None);
                crate::completion::notify_completion_update();
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
                update_diagnostics_from_cache(&cache, None);
            }
        }
    });
}

fn spawn_external_refresh<F>(
    cache: Arc<RwLock<ProjectDynamicCache>>,
    cache_key: ExternalCompletionCacheKey,
    loader: F,
) where
    F: FnOnce() -> Result<Vec<EnhancedCandidate>> + Send + 'static,
{
    std::thread::spawn(move || {
        let result = loader();
        let mut cache = cache.write();
        cache.external_pending.remove(&cache_key);
        match result {
            Ok(candidates) => {
                if candidates.is_empty() {
                    update_diagnostics_from_cache(
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
                    update_diagnostics_from_cache(&cache, Some("external refresh ok".to_string()));
                    crate::completion::notify_completion_update();
                }
            }
            Err(err) => {
                warn!("External completer refresh failed: {}", err);
                update_diagnostics_from_cache(
                    &cache,
                    Some(format!("external refresh error: {err}")),
                );
            }
        }
    });
}

fn insert_external_cache_entry(
    cache: &mut ProjectDynamicCache,
    cache_key: ExternalCompletionCacheKey,
    entry: ExternalCompletionCacheEntry,
) {
    cache.external.insert(cache_key, entry);
    prune_external_cache(cache);
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

fn update_diagnostics_from_cache(cache: &ProjectDynamicCache, last_external: Option<String>) {
    let mut diagnostics = DYNAMIC_COMPLETION_DIAGNOSTICS.write();
    diagnostics.command_entries = cache.commands.len();
    diagnostics.command_pending = cache.command_pending.len();
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

fn task_completion_signature(project_root: &Path) -> Vec<FileMetadataSignature> {
    [
        "mise.toml",
        "Taskfile.yml",
        "Taskfile.yaml",
        "turbo.json",
        "project.json",
        "package.json",
        "Cargo.toml",
        "Makefile",
        "makefile",
        "deno.json",
        "deno.jsonc",
    ]
    .into_iter()
    .map(|name| file_metadata_signature(&project_root.join(name)))
    .collect()
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
    let mut after_compose = false;
    let mut skip_next_value = false;

    for token in completion_words(parsed_command_line) {
        if skip_next_value {
            skip_next_value = false;
            continue;
        }

        if !after_compose {
            if token == "compose" {
                after_compose = true;
            }
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
    let mut after_compose = false;
    let words = completion_words(parsed_command_line);

    for (index, token) in words.iter().enumerate() {
        if !after_compose {
            if *token == "compose" {
                after_compose = true;
            }
            continue;
        }

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
    let mut command = subprocess::shell_command(&key.command_template);
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
        .filter_map(|line| parse_external_completion_line(&line, &key.current_token))
        .collect())
}

fn run_fish_completer_for_key(
    command_path: &str,
    key: &ExternalCompletionCacheKey,
) -> Result<Vec<EnhancedCandidate>> {
    let mut command = subprocess::command(command_path);
    command
        .arg("-c")
        .arg("complete -C \"$argv[1]\"")
        .arg("--")
        .arg(&key.input)
        .current_dir(&key.current_dir);

    let lines = collect_command_lines(command)?;
    Ok(lines
        .into_iter()
        .filter_map(|line| parse_fish_completion_line(&line, &key.current_token))
        .collect())
}

fn parse_external_completion_line(line: &str, current_token: &str) -> Option<EnhancedCandidate> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('{')
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(object) = value.as_object()
    {
        let text = object.get("text").and_then(|value| value.as_str())?;
        let replacement = object
            .get("replacement")
            .and_then(|value| value.as_str())
            .unwrap_or(text)
            .trim();
        if replacement.is_empty() || !matches_prefix(current_token, replacement) {
            return None;
        }

        let mut description = object
            .get("description")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if description.is_none() && replacement != text {
            description = Some(text.to_string());
        }

        let candidate_type = object
            .get("type")
            .and_then(|value| value.as_str())
            .and_then(parse_external_candidate_type)
            .unwrap_or(CandidateType::Argument);
        let priority = object
            .get("priority")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(200);

        return Some(EnhancedCandidate {
            text: replacement.to_string(),
            description,
            candidate_type,
            priority,
        });
    }

    let (text, description) = if let Some((text, description)) = trimmed.split_once('\t') {
        (text.trim(), Some(description.trim().to_string()))
    } else {
        (trimmed, None)
    };

    if text.is_empty() || !matches_fish_prefix(current_token, text) {
        return None;
    }

    Some(EnhancedCandidate {
        text: text.to_string(),
        description,
        candidate_type: CandidateType::Argument,
        priority: 200,
    })
}

fn parse_external_candidate_type(value: &str) -> Option<CandidateType> {
    match value {
        "subcommand" | "SubCommand" => Some(CandidateType::SubCommand),
        "short-option" | "short_option" | "ShortOption" => Some(CandidateType::ShortOption),
        "long-option" | "long_option" | "LongOption" => Some(CandidateType::LongOption),
        "argument" | "Argument" => Some(CandidateType::Argument),
        "file" | "File" => Some(CandidateType::File),
        "directory" | "Directory" => Some(CandidateType::Directory),
        "process" | "Process" => Some(CandidateType::Process),
        "generic" | "Generic" => Some(CandidateType::Generic),
        _ => None,
    }
}

fn parse_fish_completion_line(line: &str, current_token: &str) -> Option<EnhancedCandidate> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (text, description) = if let Some((text, description)) = trimmed.split_once('\t') {
        (text.trim(), Some(description.trim().to_string()))
    } else {
        (trimmed, None)
    };

    if text.is_empty() || !matches_fish_prefix(current_token, text) {
        return None;
    }

    let candidate_type = if text.ends_with('/') {
        CandidateType::Directory
    } else if text.starts_with("--") {
        CandidateType::LongOption
    } else if text.starts_with('-') {
        CandidateType::ShortOption
    } else {
        CandidateType::Argument
    };

    Some(EnhancedCandidate {
        text: text.to_string(),
        description,
        candidate_type,
        // Keep fish as a broad, low-priority fallback so built-in JSON and
        // project-aware dynamic providers win when both sources know a value.
        priority: 35,
    })
}

fn matches_fish_prefix(current_token: &str, text: &str) -> bool {
    if matches_prefix(current_token, text) || text.starts_with(current_token) {
        return true;
    }

    let quote_stripped = current_token.trim_start_matches(['\'', '"']);
    if quote_stripped != current_token
        && (matches_prefix(quote_stripped, text) || text.starts_with(quote_stripped))
    {
        return true;
    }

    let normalized_current_token = normalize_path_token(current_token);
    normalized_current_token != current_token
        && (matches_prefix(&normalized_current_token, text)
            || text.starts_with(&normalized_current_token))
}

fn run_command_stdout(command_path: &str, args: &[&str], current_dir: &Path) -> Result<String> {
    let mut command = subprocess::command(command_path);
    command.args(args).current_dir(current_dir);
    subprocess::collect_stdout(command, COMPLETION_COMMAND_TIMEOUT)
}

fn run_command_lines(command_path: &str, args: &[&str], current_dir: &Path) -> Result<Vec<String>> {
    let mut command = subprocess::command(command_path);
    command.args(args).current_dir(current_dir);
    collect_command_lines(command)
}

fn collect_command_lines(command: std::process::Command) -> Result<Vec<String>> {
    Ok(
        subprocess::collect_stdout(command, COMPLETION_COMMAND_TIMEOUT)?
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    )
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

fn parse_container_images(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .map(|line| line.trim())
            .filter(|image| !image.contains("<none>"))
            .map(str::to_string)
            .collect(),
    )
}

fn parse_first_fields(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split_whitespace().next().map(str::to_string))
            .collect(),
    )
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

fn selected_git_remote(parsed_command_line: &ParsedCommandLine) -> Option<&str> {
    parsed_command_line
        .specified_arguments
        .first()
        .map(String::as_str)
        .filter(|remote| !remote.is_empty())
        .filter(|remote| *remote != parsed_command_line.current_token)
}

fn completion_words(parsed_command_line: &ParsedCommandLine) -> Vec<&str> {
    parsed_command_line
        .subcommand_path
        .iter()
        .chain(parsed_command_line.raw_args.iter())
        .map(String::as_str)
        .collect()
}

fn kubectl_positional_words(parsed_command_line: &ParsedCommandLine) -> Vec<&str> {
    let mut positionals = Vec::new();
    let mut skip_next_value = false;

    for token in completion_words(parsed_command_line) {
        if skip_next_value {
            skip_next_value = false;
            continue;
        }

        if kubectl_option_takes_value(token) {
            skip_next_value = true;
            continue;
        }

        if is_inline_kubectl_option_value(token) || token.starts_with('-') {
            continue;
        }

        positionals.push(token);
    }

    positionals
}

fn selected_kubectl_resource(parsed_command_line: &ParsedCommandLine) -> Option<&str> {
    let current_token = parsed_command_line.current_token.as_str();
    let words = kubectl_positional_words(parsed_command_line);
    let command = words.first().copied()?;
    if !matches!(
        command,
        "get" | "describe" | "delete" | "edit" | "create" | "apply"
    ) {
        return None;
    }

    let resource = words.get(1).copied()?;
    if resource == current_token || resource.contains('/') {
        return None;
    }

    Some(resource)
}

fn selected_kubectl_namespace(parsed_command_line: &ParsedCommandLine) -> Option<&str> {
    let words = completion_words(parsed_command_line);
    for (index, token) in words.iter().enumerate() {
        if *token == "-n" || *token == "--namespace" {
            let Some(value) = words.get(index + 1).copied() else {
                continue;
            };
            if !value.is_empty() && !value.starts_with('-') {
                return Some(value);
            }
        }

        if let Some(value) = token
            .strip_prefix("--namespace=")
            .or_else(|| token.strip_prefix("-n="))
            .or_else(|| token.strip_prefix("-n").filter(|value| !value.is_empty()))
            && !value.is_empty()
        {
            return Some(value);
        }
    }

    None
}

fn split_kubectl_resource_name_token(token: &str) -> Option<(&str, &str)> {
    let (resource, name_prefix) = token.split_once('/')?;
    if resource.is_empty() {
        return None;
    }
    Some((resource, name_prefix))
}

fn kubectl_option_takes_value(token: &str) -> bool {
    matches!(
        token,
        "-n" | "--namespace"
            | "--context"
            | "--kubeconfig"
            | "-o"
            | "--output"
            | "-l"
            | "--selector"
            | "--field-selector"
            | "-f"
            | "--filename"
            | "-k"
            | "--kustomize"
            | "--as"
            | "--as-group"
            | "--cluster"
            | "--server"
            | "--token"
            | "--user"
    )
}

fn is_inline_kubectl_option_value(token: &str) -> bool {
    token.starts_with("--namespace=")
        || token.starts_with("-n=")
        || (token.starts_with("-n") && token.len() > 2)
        || token.starts_with("--context=")
        || token.starts_with("--kubeconfig=")
        || token.starts_with("--output=")
        || token.starts_with("--selector=")
        || token.starts_with("--field-selector=")
        || token.starts_with("--filename=")
        || token.starts_with("--kustomize=")
        || token.starts_with("--as=")
        || token.starts_with("--as-group=")
        || token.starts_with("--cluster=")
        || token.starts_with("--server=")
        || token.starts_with("--token=")
        || token.starts_with("--user=")
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

fn project_task_completion_config(
    scope: Option<&str>,
    parsed_command_line: &ParsedCommandLine,
) -> Option<ProjectTaskCompletionConfig> {
    if let Some(scope_sources) = scope.and_then(project_task_sources_for_scope) {
        return Some(ProjectTaskCompletionConfig {
            sources: scope_sources,
            candidate_text: project_task_candidate_text_for_scope(scope),
        });
    }

    let sources: &'static [&'static str] = match parsed_command_line.command.as_str() {
        "npm" | "pnpm" | "yarn" | "bun" => Some(JS_PROJECT_TASK_SOURCES),
        "deno" => Some(DENO_PROJECT_TASK_SOURCES),
        "turbo" => Some(TURBO_PROJECT_TASK_SOURCES),
        "nx" => Some(NX_PROJECT_TASK_SOURCES),
        "mise" => Some(MISE_PROJECT_TASK_SOURCES),
        "task" => Some(TASKFILE_PROJECT_TASK_SOURCES),
        "just" => Some(JUST_PROJECT_TASK_SOURCES),
        "make" => Some(MAKE_PROJECT_TASK_SOURCES),
        _ => None,
    }?;
    Some(ProjectTaskCompletionConfig {
        sources,
        candidate_text: ProjectTaskCandidateText::Name,
    })
}

fn project_task_sources_for_scope(scope: &str) -> Option<&'static [&'static str]> {
    match scope {
        "js" | "package-json" | "npm" | "pnpm" | "yarn" | "bun" => Some(JS_PROJECT_TASK_SOURCES),
        "deno" => Some(DENO_PROJECT_TASK_SOURCES),
        "turbo" => Some(TURBO_PROJECT_TASK_SOURCES),
        "nx" | "nx.run" => Some(NX_PROJECT_TASK_SOURCES),
        "mise" => Some(MISE_PROJECT_TASK_SOURCES),
        "taskfile" | "task" => Some(TASKFILE_PROJECT_TASK_SOURCES),
        "just" => Some(JUST_PROJECT_TASK_SOURCES),
        "make" => Some(MAKE_PROJECT_TASK_SOURCES),
        _ => None,
    }
}

fn project_task_candidate_text_for_scope(scope: Option<&str>) -> ProjectTaskCandidateText {
    match scope {
        Some("nx.run") => ProjectTaskCandidateText::NxRunArgument,
        _ => ProjectTaskCandidateText::Name,
    }
}

fn project_task_candidate_text(
    task: &task::TaskInfo,
    candidate_text: ProjectTaskCandidateText,
) -> String {
    match candidate_text {
        ProjectTaskCandidateText::Name => task.name.clone(),
        ProjectTaskCandidateText::NxRunArgument => task
            .command
            .strip_prefix("nx run ")
            .unwrap_or(&task.name)
            .to_string(),
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

fn parse_git_remote_branches(lines: &[String], remote: Option<&str>) -> Vec<String> {
    let mut values = Vec::new();
    for line in lines {
        if line.ends_with("/HEAD") || line == "HEAD" {
            continue;
        }
        let Some((candidate_remote, branch)) = line.split_once('/') else {
            continue;
        };
        if branch.is_empty() {
            continue;
        }
        if let Some(remote) = remote
            && !remote.is_empty()
            && candidate_remote != remote
        {
            continue;
        }
        values.push(branch.to_string());
    }
    dedup_sorted(values)
}

fn parse_git_stash_refs(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split(':').next().map(str::to_string))
            .collect(),
    )
}

fn parse_git_status_porcelain_paths(output: &str) -> Vec<String> {
    let records = output
        .split('\0')
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut values = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if record.len() < 4 {
            index += 1;
            continue;
        }
        let status = &record[..2];
        let path = record[3..].trim();
        if !path.is_empty() {
            values.push(path.to_string());
        }
        if status.contains('R') || status.contains('C') {
            index += 2;
        } else {
            index += 1;
        }
    }
    dedup_sorted(values)
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
            CargoMetadataValueKind::Bin | CargoMetadataValueKind::Example => {
                let Some(targets) = package
                    .get("targets")
                    .and_then(|targets| targets.as_array())
                else {
                    continue;
                };
                let expected_kind = match kind {
                    CargoMetadataValueKind::Bin => "bin",
                    CargoMetadataValueKind::Example => "example",
                    CargoMetadataValueKind::Package => unreachable!(),
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
    if command_name == "rsync" {
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

fn load_process_names() -> Vec<String> {
    let mut values = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !file_name.chars().all(|ch| ch.is_ascii_digit()) {
                continue;
            }
            if let Ok(comm) = fs::read_to_string(path.join("comm")) {
                values.push(comm.trim().to_string());
            }
        }
    }
    dedup_sorted(values)
}

fn load_process_ids() -> Vec<String> {
    let mut values = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if file_name.chars().all(|ch| ch.is_ascii_digit()) {
                values.push(file_name.to_string());
            }
        }
    }
    dedup_sorted(values)
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

fn load_network_interfaces() -> Vec<String> {
    fs::read_dir("/sys/class/net")
        .map(|entries| {
            dedup_sorted(
                entries
                    .flatten()
                    .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
                    .collect(),
            )
        })
        .unwrap_or_default()
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

fn load_sysctl_keys() -> Vec<String> {
    let root = Path::new("/proc/sys");
    let mut values = Vec::new();
    collect_sysctl_keys(root, root, &mut values);
    dedup_sorted(values)
}

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
mod tests {
    use super::*;
    use crate::completion::parser::CommandLineParser;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn parsed(input: &str) -> ParsedCommandLine {
        CommandLineParser::new().parse(input, input.len())
    }

    fn write_executable_script(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn parse_cargo_metadata_values_extracts_packages_and_targets() {
        let metadata = r#"{
          "packages": [
            {
              "name": "app",
              "targets": [
                { "name": "app", "kind": ["bin"] },
                { "name": "demo", "kind": ["example"] },
                { "name": "app", "kind": ["lib"] }
              ]
            },
            {
              "name": "lib",
              "targets": [
                { "name": "tool", "kind": ["bin"] }
              ]
            }
          ]
        }"#;

        assert_eq!(
            parse_cargo_metadata_values(metadata, CargoMetadataValueKind::Package),
            vec!["app".to_string(), "lib".to_string()]
        );
        assert_eq!(
            parse_cargo_metadata_values(metadata, CargoMetadataValueKind::Bin),
            vec!["app".to_string(), "tool".to_string()]
        );
        assert_eq!(
            parse_cargo_metadata_values(metadata, CargoMetadataValueKind::Example),
            vec!["demo".to_string()]
        );
    }

    #[test]
    fn parse_ssh_hosts_filters_patterns_and_known_host_ports() {
        let config_hosts = parse_ssh_config_hosts(
            "Host dev prod-*\n  HostName example.com\nHost staging\nHost !blocked wildcard?\n",
        );
        assert_eq!(config_hosts, vec!["dev".to_string(), "staging".to_string()]);

        let known_hosts = parse_known_hosts(
            "github.com,140.82.112.3 ssh-ed25519 AAA\n[dev.local]:2222 ssh-rsa BBB\n|1|hashed entry\n",
        );
        assert_eq!(
            known_hosts,
            vec![
                "140.82.112.3".to_string(),
                "dev.local".to_string(),
                "github.com".to_string()
            ]
        );

        assert_eq!(
            format_ssh_host_candidate_text("ssh", Some("alice"), "dev".to_string()),
            "alice@dev"
        );
        assert_eq!(
            format_ssh_host_candidate_text("rsync", Some("alice"), "dev".to_string()),
            "alice@dev:"
        );
        assert_eq!(
            format_ssh_host_candidate_text("rsync", None, "dev".to_string()),
            "dev:"
        );
    }

    #[test]
    fn command_value_parsers_normalize_common_command_outputs() {
        let unit_lines = vec![
            "ssh.service enabled".to_string(),
            "docker.service loaded active running Docker".to_string(),
            "ssh.service enabled".to_string(),
        ];
        assert_eq!(
            parse_first_fields(&unit_lines),
            vec!["docker.service".to_string(), "ssh.service".to_string()]
        );

        assert_eq!(
            parse_screen_sessions(&[
                "There is a screen on:".to_string(),
                "\t1234.dev-session\t(Detached)".to_string(),
                "1 Socket in /run/screen/S-user.".to_string(),
            ]),
            vec!["1234.dev-session".to_string()]
        );

        assert_eq!(
            parse_pip_freeze_packages(&[
                "requests==2.32.0".to_string(),
                "pytest==8.0.0".to_string(),
            ]),
            vec!["pytest".to_string(), "requests".to_string()]
        );
        assert_eq!(
            parse_package_lines(&[
                " bash ".to_string(),
                "".to_string(),
                "coreutils".to_string(),
            ]),
            vec!["bash".to_string(), "coreutils".to_string()]
        );
        assert_eq!(
            parse_blkid_export_attribute(
                &[
                    "DEVNAME=/dev/sda1".to_string(),
                    "UUID=abcd-1234".to_string(),
                    "LABEL=rootfs".to_string(),
                    "UUID=efgh-5678".to_string(),
                ],
                "UUID",
            ),
            vec!["abcd-1234".to_string(), "efgh-5678".to_string()]
        );
        assert_eq!(
            parse_busctl_services(&[
                "NAME PID PROCESS USER CONNECTION UNIT SESSION DESCRIPTION".to_string(),
                "org.freedesktop.login1 1 systemd root - - - Login".to_string(),
                ":1.10 100 demo user - - - App".to_string(),
            ]),
            vec![":1.10".to_string(), "org.freedesktop.login1".to_string()]
        );
        assert_eq!(
            parse_loginctl_sessions(&[
                "SESSION UID USER SEAT TTY".to_string(),
                "2 1000 alice seat0 tty2".to_string(),
            ]),
            vec!["2".to_string()]
        );
        assert_eq!(
            parse_losetup_devices(&["NAME".to_string(), "loop0".to_string()]),
            vec!["/dev/loop0".to_string()]
        );

        assert_eq!(
            parse_nmcli_first_field(&[
                "home-wifi:802-11-wireless".to_string(),
                "eth0:ethernet".to_string(),
            ]),
            vec!["eth0".to_string(), "home-wifi".to_string()]
        );

        assert_eq!(
            parse_nmcli_connected_devices(&[
                "wlan0:connected".to_string(),
                "eth0:disconnected".to_string(),
                "lo:unmanaged".to_string(),
            ]),
            vec!["wlan0".to_string()]
        );

        assert_eq!(
            parse_lsblk_devices(&[
                "sda disk".to_string(),
                "sda1 part".to_string(),
                "sr0 rom".to_string(),
            ]),
            vec!["/dev/sda".to_string(), "/dev/sda1".to_string()]
        );
        assert_eq!(
            parse_fstab_mountpoints(
                "# comment\nUUID=abc / ext4 defaults 0 1\nserver:/share /mnt/with\\040space nfs defaults 0 0\n"
            ),
            vec!["/".to_string(), "/mnt/with space".to_string()]
        );

        assert_eq!(
            parse_git_remote_branches(
                &[
                    "origin/main".to_string(),
                    "upstream/dev".to_string(),
                    "origin/HEAD".to_string(),
                ],
                Some("origin"),
            ),
            vec!["main".to_string()]
        );
        assert_eq!(
            parse_git_stash_refs(&[
                "stash@{0}: WIP on main".to_string(),
                "stash@{1}: On dev".to_string(),
            ]),
            vec!["stash@{0}".to_string(), "stash@{1}".to_string()]
        );
        assert_eq!(
            parse_git_status_porcelain_paths(" M src/lib.rs\0R  src/new.rs\0src/old.rs\0"),
            vec!["src/lib.rs".to_string(), "src/new.rs".to_string()]
        );
    }

    #[test]
    fn wireguard_config_names_strip_conf_suffix_and_ignore_other_files() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("wg0.conf"), "").unwrap();
        fs::write(dir.path().join("wg-dev.conf"), "").unwrap();
        fs::write(dir.path().join("notes.txt"), "").unwrap();
        fs::create_dir(dir.path().join("nested.conf")).unwrap();

        assert_eq!(
            collect_wireguard_config_names_from_dirs([dir.path()]),
            vec!["wg-dev".to_string(), "wg0".to_string()]
        );
    }

    #[test]
    fn journalctl_user_unit_completion_queries_user_systemd_units() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        write_executable_script(
            &bin_dir.join("systemctl"),
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > systemctl-args.txt\nprintf 'ssh.service loaded active running SSH\\n'\n",
        );

        let environment = Environment::new();
        {
            let mut env = environment.write();
            env.paths = vec![bin_dir.display().to_string()];
            env.clear_command_cache();
        }
        let provider = DynamicCompletionProvider::new(environment);
        let candidates = provider.collect_declared_dynamic_candidates(
            "systemctl.unit",
            None,
            &parsed("journalctl --user-unit ssh"),
            dir.path(),
            false,
        );

        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.text == "ssh.service"),
            "expected user unit candidate in {:?}",
            candidates
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("systemctl-args.txt")).unwrap(),
            "--user\nlist-units\n--all\n--no-pager\n--no-legend\n"
        );
    }

    #[test]
    fn package_json_dependency_parser_collects_all_dependency_groups() {
        let dir = tempdir().unwrap();
        let package_json = dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
                "dependencies": { "react": "latest" },
                "devDependencies": { "vite": "latest" },
                "optionalDependencies": { "fsevents": "latest" },
                "peerDependencies": { "@types/react": "latest" }
            }"#,
        )
        .unwrap();

        assert_eq!(
            load_package_json_dependencies(&package_json),
            vec![
                "@types/react".to_string(),
                "fsevents".to_string(),
                "react".to_string(),
                "vite".to_string()
            ]
        );
    }

    #[test]
    fn dynamic_command_cache_is_scoped_by_command_and_kind() {
        let provider = DynamicCompletionProvider::new(Environment::new());
        let scope = PathBuf::from("/tmp/dsh-cache-scope");

        let first = provider.collect_cached_value_candidates(
            "alpha",
            "value",
            scope.clone(),
            "o",
            "test",
            false,
            || Ok(vec!["one".to_string()]),
        );
        let second = provider.collect_cached_value_candidates(
            "beta",
            "value",
            scope.clone(),
            "t",
            "test",
            false,
            || Ok(vec!["two".to_string()]),
        );
        let different_kind = provider.collect_cached_value_candidates(
            "alpha",
            "other",
            scope,
            "t",
            "test",
            false,
            || Ok(vec!["three".to_string()]),
        );

        assert_eq!(first[0].text, "one");
        assert_eq!(second[0].text, "two");
        assert_eq!(different_kind[0].text, "three");
    }

    #[test]
    fn cached_only_dynamic_values_do_not_run_loader_on_miss() {
        let provider = DynamicCompletionProvider::new(Environment::new());
        let candidates = provider.collect_cached_value_candidates(
            "ghost",
            "value",
            PathBuf::from("/tmp/dsh-cached-only"),
            "",
            "test",
            true,
            || panic!("cached-only lookup must not invoke loader"),
        );

        assert!(candidates.is_empty());
    }

    #[test]
    fn mount_cached_candidates_include_fstab_mountpoints() {
        let provider = DynamicCompletionProvider::new(Environment::new());
        provider.cache.write().commands.insert(
            DynamicCommandCacheKey {
                kind: DynamicCommandCacheKind::CommandValue {
                    command: "fstab".to_string(),
                    value_kind: "mountpoint".to_string(),
                },
                scope_dir: PathBuf::from("/etc/fstab"),
            },
            CommandValueCacheEntry {
                values: vec!["/mnt/data".to_string(), "/srv/share".to_string()],
                cached_at: Instant::now(),
                last_load_duration: None,
                last_error: None,
            },
        );

        let candidates =
            provider.collect_mount_candidates_cached(&parsed("mount /mnt"), Path::new("/tmp"));

        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.text == "/mnt/data")
        );
    }

    #[test]
    fn parse_compose_services_reads_top_level_service_names() {
        let dir = tempdir().unwrap();
        let compose_file = dir.path().join("compose.yaml");
        fs::write(
            &compose_file,
            r#"
services:
  api:
    image: example/api
  worker:
    build: .
volumes:
  cache:
"#,
        )
        .unwrap();

        let services = parse_compose_service_names(&compose_file).unwrap();
        assert_eq!(services, vec!["api".to_string(), "worker".to_string()]);
    }

    #[test]
    fn task_completion_cache_refreshes_when_taskfile_changes() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("apps").join("api");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n  build:\n    cmds:\n      - cargo build\n",
        )
        .unwrap();

        let provider = DynamicCompletionProvider::new(Environment::new());
        let build_candidates = provider.collect_task_candidates(&parsed("task bu"), &nested);

        assert!(
            build_candidates
                .iter()
                .any(|candidate| candidate.text == "build"),
            "expected task completion from project root"
        );

        std::thread::sleep(Duration::from_millis(20));
        fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n  build:\n    cmds:\n      - cargo build\n  test:\n    cmds:\n      - cargo test\n",
        )
        .unwrap();

        let test_candidates = provider.collect_task_candidates(&parsed("task te"), &nested);

        assert!(
            test_candidates
                .iter()
                .any(|candidate| candidate.text == "test"),
            "expected task cache invalidation after Taskfile change"
        );
    }

    #[test]
    fn project_task_cached_only_reuses_existing_cache_without_loading() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n  build:\n    cmds:\n      - cargo build\n",
        )
        .unwrap();

        let provider = DynamicCompletionProvider::new(Environment::new());
        assert!(
            provider
                .collect_task_candidates_with_mode(&parsed("bun run bu"), dir.path(), true)
                .is_empty(),
            "cached-only task completion should not load on miss"
        );

        let loaded = provider.collect_task_candidates(&parsed("bun run bu"), dir.path());
        assert!(loaded.iter().any(|candidate| candidate.text == "build"));

        let cached =
            provider.collect_task_candidates_with_mode(&parsed("bun run bu"), dir.path(), true);
        assert!(cached.iter().any(|candidate| candidate.text == "build"));
    }

    #[test]
    fn compose_service_cache_refreshes_when_compose_file_changes() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("services").join("api");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            dir.path().join("compose.yaml"),
            "services:\n  api:\n    image: example/api\n",
        )
        .unwrap();

        let provider = DynamicCompletionProvider::new(Environment::new());
        let api_candidates = provider.collect_compose_service_candidates(&nested, "ap", None);

        assert!(
            api_candidates
                .iter()
                .any(|candidate| candidate.text == "api"),
            "expected compose service completion from ancestor compose file"
        );

        std::thread::sleep(Duration::from_millis(20));
        fs::write(
            dir.path().join("compose.yaml"),
            "services:\n  api:\n    image: example/api\n  worker:\n    image: example/worker\n",
        )
        .unwrap();

        let worker_candidates = provider.collect_compose_service_candidates(&nested, "wo", None);
        assert!(
            worker_candidates
                .iter()
                .any(|candidate| candidate.text == "worker"),
            "expected compose cache invalidation after file change"
        );
    }

    #[test]
    fn docker_compose_service_completion_uses_explicit_file_option() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("compose.yaml"),
            "services:\n  api:\n    image: example/api\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("compose.dev.yml"),
            "services:\n  worker:\n    image: example/worker\n",
        )
        .unwrap();

        let provider = DynamicCompletionProvider::new(Environment::new());
        let candidates = provider.collect_docker_candidates(
            &parsed("docker compose -f compose.dev.yml up wo"),
            dir.path(),
        );

        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.text == "worker"),
            "expected service completion from docker compose -f file"
        );
        assert!(
            candidates.iter().all(|candidate| candidate.text != "api"),
            "explicit compose file should take precedence over ancestor defaults"
        );
    }

    #[test]
    fn kubectl_resource_name_completion_respects_namespace_and_resource_name_token() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let kubectl = bin_dir.join("kubectl");
        write_executable_script(
            &kubectl,
            "#!/bin/sh\n\
             printf '%s\\n' \"$@\" > kubectl-args.txt\n\
             resource=''\n\
             if [ \"$1\" = get ] && [ \"$2\" = -n ]; then\n\
               resource=\"$4\"\n\
             elif [ \"$1\" = get ]; then\n\
               resource=\"$2\"\n\
             fi\n\
             case \"$resource\" in\n\
               pod|pods) printf 'api\\nworker\\n' ;;\n\
             esac\n",
        );
        let environment = Environment::new();
        {
            let mut env = environment.write();
            env.paths = vec![bin_dir.display().to_string()];
            env.clear_command_cache();
        }
        let provider = DynamicCompletionProvider::new(environment);

        let names =
            provider.collect_kubectl_candidates(&parsed("kubectl get -n prod pods ap"), dir.path());
        assert!(
            names.iter().any(|candidate| candidate.text == "api"),
            "expected resource names to be queried inside the selected namespace"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("kubectl-args.txt")).unwrap(),
            "get\n-n\nprod\npods\n-o\njsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}\n"
        );

        let resource_name =
            provider.collect_kubectl_candidates(&parsed("kubectl get pods/ap"), dir.path());
        assert!(
            resource_name
                .iter()
                .any(|candidate| candidate.text == "pods/api"),
            "expected resource/name token completion to preserve the resource prefix"
        );
    }

    #[test]
    fn git_branch_cache_is_shared_per_project_root_and_expires_by_ttl() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("repo");
        let nested = root.join("apps").join("web");
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();
        fs::write(root.join("package.json"), "{\"name\":\"demo\"}\n").unwrap();

        let counter = dir.path().join("git-count");
        let git = bin_dir.join("git");
        fs::write(
            &git,
            format!(
                "#!/bin/sh\ncount_file=\"{}\"\ncount=0\nif [ -f \"$count_file\" ]; then\n  count=$(cat \"$count_file\")\nfi\ncount=$((count + 1))\nprintf '%s' \"$count\" > \"$count_file\"\nif [ \"$1\" = \"for-each-ref\" ]; then\n  printf 'feature/cache\\nmain\\n'\nfi\n",
                counter.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&git).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&git, permissions).unwrap();

        let environment = Environment::new();
        {
            let mut env = environment.write();
            env.paths = vec![bin_dir.display().to_string()];
            env.clear_command_cache();
        }
        let provider = DynamicCompletionProvider::new(environment);

        let first_candidates = provider.collect_git_branch_candidates(&root, "fe", false);
        assert!(
            first_candidates
                .iter()
                .any(|candidate| candidate.text == "feature/cache"),
            "first miss should synchronously fetch git branch completion"
        );

        let nested_candidates = provider.collect_git_branch_candidates(&nested, "ma", false);
        assert!(
            nested_candidates
                .iter()
                .any(|candidate| candidate.text == "main"),
            "expected cached git branch completion from nested cwd"
        );

        assert_eq!(
            fs::read_to_string(&counter).unwrap(),
            "1",
            "git command should run once within the same project root"
        );

        std::thread::sleep(Duration::from_millis(DYNAMIC_COMMAND_CACHE_TTL_MS + 50));
        let refreshed_candidates = provider.collect_git_branch_candidates(&nested, "fe", false);
        assert!(
            refreshed_candidates
                .iter()
                .any(|candidate| candidate.text == "feature/cache"),
            "expected stale git branch completion while ttl refresh runs"
        );

        assert!(
            wait_until(Duration::from_secs(2), || fs::read_to_string(&counter)
                .is_ok_and(|count| count == "2")),
            "git command should run again after ttl expiry"
        );
    }

    #[test]
    fn external_completer_parses_filters_and_receives_context() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("external-completer.sh");
        write_executable_script(
            &script,
            "#!/bin/sh\n\
             {\n\
             printf 'input=%s\\n' \"$DSH_COMPLETION_INPUT\"\n\
             printf 'cursor=%s\\n' \"$DSH_COMPLETION_CURSOR\"\n\
             printf 'command=%s\\n' \"$DSH_COMPLETION_COMMAND\"\n\
             printf 'token=%s\\n' \"$DSH_COMPLETION_CURRENT_TOKEN\"\n\
             } > external-env.txt\n\
             printf 'zzext-alpha\\tExternal alpha\\n'\n\
             printf 'other-candidate\\tOther candidate\\n'\n",
        );

        let environment = Environment::new();
        environment.write().variables.insert(
            "DSH_EXTERNAL_COMPLETER".to_string(),
            script.display().to_string(),
        );
        let provider = DynamicCompletionProvider::new(environment);
        let input = "unknown-command zzext";

        let candidates =
            provider.collect_external_candidates(dir.path(), input, input.len(), &parsed(input));

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].text, "zzext-alpha");
        assert_eq!(candidates[0].description.as_deref(), Some("External alpha"));
        assert_eq!(candidates[0].candidate_type, CandidateType::Argument);
        assert_eq!(candidates[0].priority, 200);

        assert_eq!(
            fs::read_to_string(dir.path().join("external-env.txt")).unwrap(),
            "input=unknown-command zzext\ncursor=21\ncommand=unknown-command\ntoken=zzext\n"
        );
    }

    #[test]
    fn external_completer_parses_jsonl_candidates_and_legacy_fallback() {
        let json_candidate = parse_external_completion_line(
            r#"{"text":"display alpha","description":"JSON alpha","type":"long-option","priority":240,"replacement":"--alpha"}"#,
            "--al",
        )
        .unwrap();
        assert_eq!(json_candidate.text, "--alpha");
        assert_eq!(json_candidate.description.as_deref(), Some("JSON alpha"));
        assert_eq!(json_candidate.candidate_type, CandidateType::LongOption);
        assert_eq!(json_candidate.priority, 240);

        let invalid_json_candidate = parse_external_completion_line("{not-json", "{not").unwrap();
        assert_eq!(invalid_json_candidate.text, "{not-json");
        assert_eq!(
            invalid_json_candidate.candidate_type,
            CandidateType::Argument
        );

        let legacy_candidate =
            parse_external_completion_line("zzext-alpha\tExternal alpha", "zz").unwrap();
        assert_eq!(legacy_candidate.text, "zzext-alpha");
        assert_eq!(
            legacy_candidate.description.as_deref(),
            Some("External alpha")
        );
    }

    #[test]
    fn fish_fallback_parses_tab_descriptions_and_low_priority() {
        let candidate = parse_fish_completion_line("checkout\tSwitch branches", "che").unwrap();
        assert_eq!(candidate.text, "checkout");
        assert_eq!(candidate.description.as_deref(), Some("Switch branches"));
        assert_eq!(candidate.candidate_type, CandidateType::Argument);
        assert_eq!(candidate.priority, 35);

        let option = parse_fish_completion_line("--help\tShow help", "--he").unwrap();
        assert_eq!(option.candidate_type, CandidateType::LongOption);

        assert!(matches_fish_prefix("'zz", "zz/"));
        let quoted_path = parse_fish_completion_line("zz/\tQuoted path", "'zz").unwrap();
        assert_eq!(quoted_path.candidate_type, CandidateType::Directory);
        assert_eq!(quoted_path.description.as_deref(), Some("Quoted path"));

        assert!(parse_fish_completion_line("checkout\tSwitch branches", "zz").is_none());
    }

    #[test]
    fn fish_fallback_auto_requires_fish_command_and_respects_disable() {
        let dir = tempdir().unwrap();
        let environment = Environment::new();
        {
            let mut env = environment.write();
            env.paths = vec![];
            env.clear_command_cache();
        }
        let provider = DynamicCompletionProvider::new(environment);
        let input = "git che";
        assert!(
            provider
                .collect_fish_fallback_candidates(dir.path(), input, input.len(), &parsed(input))
                .is_empty(),
            "auto fallback still needs fish on PATH"
        );

        let environment = Environment::new();
        {
            let mut env = environment.write();
            env.paths = vec![];
            env.variables
                .insert("DSH_COMPLETION_FISH_FALLBACK".to_string(), "1".to_string());
            env.clear_command_cache();
        }
        let provider = DynamicCompletionProvider::new(environment);
        assert!(
            provider
                .collect_fish_fallback_candidates(dir.path(), input, input.len(), &parsed(input))
                .is_empty(),
            "enabled fallback still needs fish on PATH"
        );

        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        write_executable_script(
            &bin_dir.join("fish"),
            "#!/bin/sh\nprintf 'checkout\\tFish checkout\\n'\n",
        );
        let disabled_environment = Environment::new();
        {
            let mut env = disabled_environment.write();
            env.paths = vec![bin_dir.display().to_string()];
            env.variables
                .insert("DSH_COMPLETION_FISH_FALLBACK".to_string(), "0".to_string());
            env.clear_command_cache();
        }
        let provider = DynamicCompletionProvider::new(disabled_environment);
        assert!(
            provider
                .collect_fish_fallback_candidates(dir.path(), input, input.len(), &parsed(input))
                .is_empty(),
            "false-like flag disables fish fallback"
        );
    }

    #[test]
    fn fish_fallback_runs_with_cursor_prefix_and_timeout() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let fish = bin_dir.join("fish");
        write_executable_script(
            &fish,
            "#!/bin/sh\nprintf '%s\\n' \"$4\" > fish-input.txt\nprintf 'zzfish-alpha\\tFish alpha\\n'\n",
        );

        let environment = Environment::new();
        {
            let mut env = environment.write();
            env.paths = vec![bin_dir.display().to_string()];
            env.clear_command_cache();
        }
        let provider = DynamicCompletionProvider::new(environment);
        let input = "unknown zzfish trailing";
        let cursor = "unknown zzfish".len();

        let parsed_at_cursor = CommandLineParser::new().parse(input, cursor);
        let candidates =
            provider.collect_fish_fallback_candidates(dir.path(), input, cursor, &parsed_at_cursor);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].text, "zzfish-alpha");
        assert_eq!(candidates[0].description.as_deref(), Some("Fish alpha"));
        assert_eq!(
            fs::read_to_string(dir.path().join("fish-input.txt")).unwrap(),
            "unknown zzfish\n"
        );

        let slow_dir = tempdir().unwrap();
        let slow_bin = slow_dir.path().join("bin");
        fs::create_dir_all(&slow_bin).unwrap();
        write_executable_script(
            &slow_bin.join("fish"),
            "#!/bin/sh\nsleep 2\nprintf 'late\\n'\n",
        );
        let slow_environment = Environment::new();
        {
            let mut env = slow_environment.write();
            env.paths = vec![slow_bin.display().to_string()];
            env.variables
                .insert("DSH_COMPLETION_FISH_FALLBACK".to_string(), "1".to_string());
            env.clear_command_cache();
        }
        let slow_provider = DynamicCompletionProvider::new(slow_environment);
        let started = Instant::now();
        let slow_candidates = slow_provider.collect_fish_fallback_candidates(
            slow_dir.path(),
            "slow la",
            "slow la".len(),
            &parsed("slow la"),
        );
        assert!(slow_candidates.is_empty());
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn timed_out_completion_command_kills_stdout_holding_descendants() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("holds-stdout.sh");
        let survived = dir.path().join("survived.txt");
        write_executable_script(
            &script,
            "#!/bin/sh\n(sleep 2; printf survived > survived.txt) &\nwait\n",
        );

        let started = Instant::now();
        let output = run_command_stdout(script.to_str().unwrap(), &[], dir.path()).unwrap();

        assert_eq!(output, "");
        assert!(started.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(1200));
        assert!(
            !survived.exists(),
            "timeout should kill descendant processes that inherited stdout"
        );
    }

    #[test]
    fn external_completer_failure_returns_empty_candidates() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("external-completer-fails.sh");
        write_executable_script(&script, "#!/bin/sh\nexit 42\n");

        let environment = Environment::new();
        environment.write().variables.insert(
            "DSH_EXTERNAL_COMPLETER".to_string(),
            script.display().to_string(),
        );
        let provider = DynamicCompletionProvider::new(environment);
        let input = "unknown-command zzext";

        let candidates =
            provider.collect_external_candidates(dir.path(), input, input.len(), &parsed(input));

        assert!(candidates.is_empty());
    }

    #[test]
    fn external_completer_returns_stale_candidates_while_refreshing() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("external-completer-refresh.sh");
        let counter = dir.path().join("external-count");
        write_executable_script(
            &script,
            &format!(
                "#!/bin/sh\n\
                 count_file=\"{}\"\n\
                 count=0\n\
                 if [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); fi\n\
                 count=$((count + 1))\n\
                 printf '%s' \"$count\" > \"$count_file\"\n\
                 if [ \"$count\" = \"1\" ]; then\n\
                 printf 'zzext-alpha\\tExternal alpha\\n'\n\
                 else\n\
                 printf 'zzext-beta\\tExternal beta\\n'\n\
                 fi\n",
                counter.display()
            ),
        );

        let environment = Environment::new();
        environment.write().variables.insert(
            "DSH_EXTERNAL_COMPLETER".to_string(),
            script.display().to_string(),
        );
        let provider = DynamicCompletionProvider::new(environment);
        let input = "unknown-command zzext";

        let first =
            provider.collect_external_candidates(dir.path(), input, input.len(), &parsed(input));
        assert_eq!(first[0].text, "zzext-alpha");

        std::thread::sleep(Duration::from_millis(DYNAMIC_COMMAND_CACHE_TTL_MS + 50));
        let stale =
            provider.collect_external_candidates(dir.path(), input, input.len(), &parsed(input));
        assert_eq!(stale[0].text, "zzext-alpha");

        assert!(
            wait_until(Duration::from_secs(2), || fs::read_to_string(&counter)
                .is_ok_and(|count| count == "2")),
            "external completer should refresh in background"
        );

        assert!(
            wait_until(Duration::from_secs(2), || {
                provider
                    .collect_external_candidates(dir.path(), input, input.len(), &parsed(input))
                    .first()
                    .is_some_and(|candidate| candidate.text == "zzext-beta")
            }),
            "external completer should expose refreshed candidates after background refresh"
        );
    }

    #[test]
    fn external_completion_cache_prunes_oldest_entries() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("external-completer-cache.sh");
        write_executable_script(
            &script,
            "#!/bin/sh\nprintf '%s-candidate\\n' \"$DSH_COMPLETION_CURRENT_TOKEN\"\n",
        );

        let environment = Environment::new();
        environment.write().variables.insert(
            "DSH_EXTERNAL_COMPLETER".to_string(),
            script.display().to_string(),
        );
        let provider = DynamicCompletionProvider::new(environment);

        for index in 0..(EXTERNAL_COMPLETION_CACHE_LIMIT + 3) {
            let input = format!("unknown-command zz{index}");
            let expected = format!("zz{index}-candidate");
            assert!(
                wait_until(Duration::from_secs(2), || provider
                    .collect_external_candidates(dir.path(), &input, input.len(), &parsed(&input))
                    .first()
                    .is_some_and(|candidate| candidate.text == expected)),
                "external cache prune setup should load candidate for {input}"
            );
        }

        let cache = provider.cache.read();
        assert_eq!(cache.external.len(), EXTERNAL_COMPLETION_CACHE_LIMIT);
        assert_eq!(cache.external_pruned_total, 3);
    }
}
