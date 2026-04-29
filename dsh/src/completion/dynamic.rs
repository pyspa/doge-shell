use super::integrated::{CandidateType, EnhancedCandidate, matches_prefix};
use super::parser::{CompletionContext, ParsedCommandLine};
use crate::environment::Environment;
use anyhow::Result;
use dsh_builtin::{project_context, task};
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tracing::warn;
use wait_timeout::ChildExt;

const DYNAMIC_COMMAND_CACHE_TTL_MS: u64 = 1000;

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

#[derive(Debug, Clone)]
struct ComposeCacheEntry {
    signature: FileMetadataSignature,
    services: Vec<String>,
}

#[derive(Debug, Clone)]
struct CommandValueCacheEntry {
    values: Vec<String>,
    cached_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum DynamicCommandCacheKind {
    GitBranch,
    GitRemote,
    GitWorktree,
    KubectlContext,
    KubectlNamespace,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DynamicCommandCacheKey {
    kind: DynamicCommandCacheKind,
    scope_dir: PathBuf,
}

#[derive(Debug, Default)]
struct ProjectDynamicCache {
    tasks: HashMap<PathBuf, TaskCacheEntry>,
    compose_services: HashMap<PathBuf, ComposeCacheEntry>,
    commands: HashMap<DynamicCommandCacheKey, CommandValueCacheEntry>,
}

pub(crate) struct DynamicCompletionProvider {
    environment: Arc<RwLock<Environment>>,
    cache: RwLock<ProjectDynamicCache>,
}

impl DynamicCompletionProvider {
    pub(crate) fn new(environment: Arc<RwLock<Environment>>) -> Self {
        Self {
            environment,
            cache: RwLock::new(ProjectDynamicCache::default()),
        }
    }

    pub(crate) fn collect_task_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        let current_token = parsed_command_line.current_token.as_str();
        match self.load_project_tasks(current_dir) {
            Ok(tasks) => tasks
                .into_iter()
                .filter(|task| matches_prefix(current_token, &task.name))
                .map(|task| EnhancedCandidate {
                    text: task.name,
                    description: Some(format_task_description(&task.source, &task.command)),
                    candidate_type: CandidateType::Argument,
                    priority: 90,
                })
                .collect(),
            Err(e) => {
                warn!("Failed to load task completions: {}", e);
                Vec::new()
            }
        }
    }

    pub(crate) fn collect_git_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        let Some(primary_subcommand) = parsed_command_line.subcommand_path.first() else {
            return Vec::new();
        };
        let current_token = parsed_command_line.current_token.as_str();
        let inferred_subcommand_arg_index =
            parsed_command_line.subcommand_path.len().saturating_sub(2);

        match parsed_command_line.completion_context {
            CompletionContext::Argument { arg_index, .. } => match primary_subcommand.as_str() {
                "checkout" | "switch" | "merge" | "rebase" => {
                    self.collect_git_branch_candidates(current_dir, current_token)
                }
                "push" | "pull" | "fetch" => {
                    if arg_index == 0 {
                        self.collect_git_remote_candidates(current_dir, current_token)
                    } else {
                        self.collect_git_branch_candidates(current_dir, current_token)
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
                            self.collect_git_remote_candidates(current_dir, current_token)
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
                        "remove" | "move" | "lock" | "unlock" | "repair" => {
                            self.collect_git_worktree_candidates(current_dir, current_token)
                        }
                        "add" if arg_index > 0 => {
                            self.collect_git_branch_candidates(current_dir, current_token)
                        }
                        _ => Vec::new(),
                    }
                }
                _ => Vec::new(),
            },
            CompletionContext::SubCommand => match primary_subcommand.as_str() {
                "checkout" | "switch" | "merge" | "rebase" => {
                    self.collect_git_branch_candidates(current_dir, current_token)
                }
                "push" | "pull" | "fetch" => {
                    if inferred_subcommand_arg_index == 0 {
                        self.collect_git_remote_candidates(current_dir, current_token)
                    } else {
                        self.collect_git_branch_candidates(current_dir, current_token)
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
                            self.collect_git_remote_candidates(current_dir, current_token)
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
                        "remove" | "move" | "lock" | "unlock" | "repair" => {
                            self.collect_git_worktree_candidates(current_dir, current_token)
                        }
                        "add" if inferred_subcommand_arg_index > 0 => {
                            self.collect_git_branch_candidates(current_dir, current_token)
                        }
                        _ => Vec::new(),
                    }
                }
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    pub(crate) fn collect_docker_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        if parsed_command_line
            .subcommand_path
            .first()
            .map(String::as_str)
            != Some("compose")
        {
            return Vec::new();
        }

        let Some(command_name) = parsed_command_line
            .subcommand_path
            .get(1)
            .map(String::as_str)
        else {
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
                    self.collect_compose_service_candidates(current_dir, current_token)
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn collect_kubectl_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        let current_token = parsed_command_line.current_token.as_str();
        match &parsed_command_line.completion_context {
            CompletionContext::OptionValue { option_name, .. } => match option_name.as_str() {
                "--context" => self.collect_kubectl_context_candidates(current_dir, current_token),
                "-n" | "--namespace" => {
                    self.collect_kubectl_namespace_candidates(current_dir, current_token)
                }
                _ => Vec::new(),
            },
            CompletionContext::SubCommand | CompletionContext::Argument { .. } => {
                let path = parsed_command_line
                    .subcommand_path
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                if path.len() >= 2 && path[0] == "config" && path[1] == "use-context" {
                    self.collect_kubectl_context_candidates(current_dir, current_token)
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
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

        match run_external_completer(
            &command_template,
            current_dir,
            input,
            cursor_pos,
            parsed_command_line,
        ) {
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
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitBranch,
            scope_dir,
            current_token,
            "git branch",
            || {
                let Some(command_path) = self.resolve_command_path("git") else {
                    return Ok(Vec::new());
                };

                run_command_lines(
                    &command_path,
                    &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
                    current_dir,
                )
            },
        )
    }

    fn collect_git_remote_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitRemote,
            scope_dir,
            current_token,
            "git remote",
            || {
                let Some(command_path) = self.resolve_command_path("git") else {
                    return Ok(Vec::new());
                };

                run_command_lines(&command_path, &["remote"], current_dir)
            },
        )
    }

    fn collect_git_worktree_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitWorktree,
            scope_dir,
            current_token,
            "git worktree",
            || {
                let Some(command_path) = self.resolve_command_path("git") else {
                    return Ok(Vec::new());
                };

                Ok(run_command_lines(
                    &command_path,
                    &["worktree", "list", "--porcelain"],
                    current_dir,
                )?
                .into_iter()
                .filter_map(|line| line.strip_prefix("worktree ").map(str::to_string))
                .collect())
            },
        )
    }

    fn collect_compose_service_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        match self.load_compose_services(current_dir) {
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

    fn collect_kubectl_context_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::KubectlContext,
            canonicalize_path(current_dir),
            current_token,
            "kubectl context",
            || {
                let Some(command_path) = self.resolve_command_path("kubectl") else {
                    return Ok(Vec::new());
                };

                run_command_lines(
                    &command_path,
                    &["config", "get-contexts", "-o", "name"],
                    current_dir,
                )
            },
        )
    }

    fn collect_kubectl_namespace_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::KubectlNamespace,
            canonicalize_path(current_dir),
            current_token,
            "kubectl namespace",
            || {
                let Some(command_path) = self.resolve_command_path("kubectl") else {
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
                    current_dir,
                )
            },
        )
    }

    fn collect_cached_command_candidates<F>(
        &self,
        kind: DynamicCommandCacheKind,
        scope_dir: PathBuf,
        current_token: &str,
        description: &str,
        loader: F,
    ) -> Vec<EnhancedCandidate>
    where
        F: FnOnce() -> Result<Vec<String>>,
    {
        match self.load_command_values(kind, scope_dir, loader) {
            Ok(values) => values
                .into_iter()
                .filter(|value| matches_prefix(current_token, value))
                .map(|value| EnhancedCandidate {
                    text: value,
                    description: Some(description.to_string()),
                    candidate_type: CandidateType::Argument,
                    priority: 130,
                })
                .collect(),
            Err(err) => {
                warn!("Failed to load {} completions: {}", description, err);
                Vec::new()
            }
        }
    }

    fn load_project_tasks(&self, current_dir: &Path) -> Result<Vec<task::TaskInfo>> {
        let project_root = project_context::find_project_root(current_dir);
        let signature = task_completion_signature(&project_root);

        if let Some(tasks) = self.lookup_task_cache(&project_root, &signature) {
            return Ok(tasks);
        }

        let tasks = task::list_tasks_in_dir(&project_root)?;
        self.cache.write().tasks.insert(
            project_root,
            TaskCacheEntry {
                signature,
                tasks: tasks.clone(),
            },
        );
        Ok(tasks)
    }

    fn lookup_task_cache(
        &self,
        project_root: &Path,
        signature: &[FileMetadataSignature],
    ) -> Option<Vec<task::TaskInfo>> {
        let cache = self.cache.read();
        let entry = cache.tasks.get(project_root)?;
        if entry.signature == signature {
            Some(entry.tasks.clone())
        } else {
            None
        }
    }

    fn load_compose_services(&self, current_dir: &Path) -> Result<Option<(PathBuf, Vec<String>)>> {
        let Some(compose_file) = find_compose_file(current_dir) else {
            return Ok(None);
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
    ) -> Result<Vec<String>>
    where
        F: FnOnce() -> Result<Vec<String>>,
    {
        let cache_key = DynamicCommandCacheKey { kind, scope_dir };
        let ttl = Duration::from_millis(DYNAMIC_COMMAND_CACHE_TTL_MS);

        {
            let cache = self.cache.read();
            if let Some(entry) = cache.commands.get(&cache_key)
                && entry.cached_at.elapsed() < ttl
            {
                return Ok(entry.values.clone());
            }
        }

        let values = loader()?;
        self.cache.write().commands.insert(
            cache_key,
            CommandValueCacheEntry {
                values: values.clone(),
                cached_at: Instant::now(),
            },
        );
        Ok(values)
    }

    fn resolve_command_path(&self, command_name: &str) -> Option<String> {
        self.environment.read().lookup(command_name)
    }
}

fn canonicalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
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

fn run_external_completer(
    command_template: &str,
    current_dir: &Path,
    input: &str,
    cursor_pos: usize,
    parsed_command_line: &ParsedCommandLine,
) -> Result<Vec<EnhancedCandidate>> {
    let subcommand_path = parsed_command_line.subcommand_path.join(" ");
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command_template)
        .current_dir(current_dir)
        .env("DSH_COMPLETION_INPUT", input)
        .env("DSH_COMPLETION_CURSOR", cursor_pos.to_string())
        .env("DSH_COMPLETION_COMMAND", &parsed_command_line.command)
        .env(
            "DSH_COMPLETION_CURRENT_TOKEN",
            &parsed_command_line.current_token,
        )
        .env("DSH_COMPLETION_SUBCOMMAND_PATH", &subcommand_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let lines = wait_and_collect_lines(&mut child)?;
    Ok(lines
        .into_iter()
        .filter_map(|line| {
            let (text, description) = if let Some((text, description)) = line.split_once('\t') {
                (text.trim(), Some(description.trim().to_string()))
            } else {
                (line.trim(), None)
            };

            if text.is_empty() || !matches_prefix(&parsed_command_line.current_token, text) {
                return None;
            }

            Some(EnhancedCandidate {
                text: text.to_string(),
                description,
                candidate_type: CandidateType::Argument,
                priority: 200,
            })
        })
        .collect())
}

fn run_command_lines(command_path: &str, args: &[&str], current_dir: &Path) -> Result<Vec<String>> {
    let mut child = Command::new(command_path)
        .args(args)
        .current_dir(current_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    wait_and_collect_lines(&mut child)
}

fn wait_and_collect_lines(child: &mut std::process::Child) -> Result<Vec<String>> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Child stdout not captured"))?;
    let reader_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        stdout.read_to_string(&mut buf)?;
        Ok::<String, std::io::Error>(buf)
    });

    match child.wait_timeout(Duration::from_millis(1500))? {
        Some(status) => {
            let output = reader_thread
                .join()
                .map_err(|_| anyhow::anyhow!("Stdout reader thread panicked"))??;
            if !status.success() {
                return Ok(Vec::new());
            }

            Ok(output
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect())
        }
        None => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader_thread.join();
            Ok(Vec::new())
        }
    }
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
        let api_candidates = provider.collect_compose_service_candidates(&nested, "ap");

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

        let worker_candidates = provider.collect_compose_service_candidates(&nested, "wo");
        assert!(
            worker_candidates
                .iter()
                .any(|candidate| candidate.text == "worker"),
            "expected compose cache invalidation after file change"
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

        let root_candidates = provider.collect_git_branch_candidates(&root, "fe");
        assert!(
            root_candidates
                .iter()
                .any(|candidate| candidate.text == "feature/cache"),
            "expected git branch completion from fake git"
        );

        let nested_candidates = provider.collect_git_branch_candidates(&nested, "ma");
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
        let refreshed_candidates = provider.collect_git_branch_candidates(&nested, "fe");
        assert!(
            refreshed_candidates
                .iter()
                .any(|candidate| candidate.text == "feature/cache"),
            "expected git branch completion after ttl refresh"
        );

        assert_eq!(
            fs::read_to_string(&counter).unwrap(),
            "2",
            "git command should run again after ttl expiry"
        );
    }
}
