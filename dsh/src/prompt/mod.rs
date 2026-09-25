use crate::environment::{ChangePwdHook, Environment};
use crate::github::GitHubStatus;
use anyhow::Result;
use crossterm::cursor;
use crossterm::queue;
use crossterm::style::Stylize;
use dsh_builtin::project_context;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

pub mod context;
mod git_status;
pub mod modules;
pub(crate) mod probe_lifecycle;
mod render;
pub(crate) mod runtime;
#[cfg(test)]
mod tests;
mod version_probes;

#[cfg(test)]
pub(crate) use git_status::parse_git_status_output;
pub use git_status::{fetch_git_status_async, fetch_git_status_sync, find_git_root_async};
pub(crate) use probe_lifecycle::{PromptProbe, PromptProbeEpoch, PromptProbeLifecycle};
pub(crate) use runtime::{PromptRuntimeIdentity, PromptRuntimeSnapshot};
#[cfg(test)]
use version_probes::kube_config_present_from;
pub(crate) use version_probes::{
    fetch_aws_profile_from, fetch_docker_context_async_from, fetch_go_version_async,
    fetch_k8s_info_async, fetch_node_version_async, fetch_python_version_async,
    fetch_rust_version_async,
};
use version_probes::{
    should_attempt_docker_context_check_from, should_attempt_k8s_context_check_with,
};

use context::PromptContext;
use modules::PromptModule;
use modules::aws::AwsModule;
use modules::directory::DirectoryModule;
use modules::docker::DockerModule;
use modules::execution_time::ExecutionTimeModule;
use modules::exit_status::ExitStatusModule;
use modules::git::GitModule;
use modules::go::GoModule;
use modules::kubernetes::KubernetesModule;
use modules::nodejs::NodeModule;
use modules::python::PythonModule;
use modules::rust::RustModule;
use modules::time::TimeModule;

// Re-export for compatibility
pub use crate::prompt::context::PromptContext as Context; // just in case

// Constants
const BRANCH_MARK: &str = "🐾";
const EXTERNAL_TOOL_BACKOFF_BASE: Duration = Duration::from_secs(5);
const EXTERNAL_TOOL_BACKOFF_MAX: Duration = Duration::from_secs(300);

impl ChangePwdHook for Arc<RwLock<Prompt>> {
    fn call(&self, pwd: &Path, _env: Arc<RwLock<Environment>>) -> Result<()> {
        self.write().set_current(pwd);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    pub branch: String,
    pub branch_status: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub oid: Option<String>,
    pub staged: u32,
    pub modified: u32,
    pub untracked: u32,
    pub conflicted: u32,
    pub renamed: u32,
    pub deleted: u32,
}

impl Default for GitStatus {
    fn default() -> Self {
        Self::new()
    }
}

impl GitStatus {
    pub fn new() -> Self {
        GitStatus {
            branch: "".to_string(),
            branch_status: None,
            ahead: 0,
            behind: 0,
            oid: None,
            staged: 0,
            modified: 0,
            untracked: 0,
            conflicted: 0,
            renamed: 0,
            deleted: 0,
        }
    }
}

/// Git status cache structure
#[derive(Debug, Clone)]
struct GitStatusCache {
    status: GitStatus,
    last_updated: Instant,
    git_root: PathBuf,
    ttl: Duration,
}

#[derive(Debug, Clone)]
struct BackoffGate {
    next_allowed: Instant,
    delay: Duration,
}

impl BackoffGate {
    fn new() -> Self {
        Self {
            next_allowed: Instant::now(),
            delay: EXTERNAL_TOOL_BACKOFF_BASE,
        }
    }

    fn should_check(&self) -> bool {
        Instant::now() >= self.next_allowed
    }

    fn record_failure(&mut self) {
        let now = Instant::now();
        self.next_allowed = now + self.delay;
        self.delay = (self.delay * 2).min(EXTERNAL_TOOL_BACKOFF_MAX);
    }

    fn reset(&mut self) {
        self.delay = EXTERNAL_TOOL_BACKOFF_BASE;
        self.next_allowed = Instant::now();
    }
}

impl GitStatusCache {
    fn new(status: GitStatus, git_root: PathBuf) -> Self {
        Self {
            status,
            last_updated: Instant::now(),
            git_root,
            ttl: Duration::from_millis(200), // Cache valid for 200ms
        }
    }

    fn is_valid(&self, current_git_root: &Path) -> bool {
        // Invalid if Git root changed
        if self.git_root != current_git_root {
            return false;
        }

        // Invalid if TTL exceeded
        self.last_updated.elapsed() < self.ttl
    }

    fn update(&mut self, status: GitStatus, git_root: PathBuf) {
        self.status = status;
        self.last_updated = Instant::now();
        self.git_root = git_root;
    }
}

#[derive(Debug)]
pub struct Prompt {
    pub current_dir: PathBuf,
    pub mark: String,
    pub github_status: Option<Arc<RwLock<GitHubStatus>>>,
    pub github_icon: String,
    current_git_root: Option<PathBuf>,
    pub needs_git_check: bool,
    git_root_cache: HashSet<String>,
    git_status_cache: Option<GitStatusCache>,
    watcher: Option<RecommendedWatcher>,
    git_sender: Option<UnboundedSender<()>>,

    // Language Support
    rust_version_cache: Option<String>,
    node_version_cache: Option<String>,
    python_version_cache: Option<String>,
    go_version_cache: Option<String>,
    rust_check_backoff: BackoffGate,
    node_check_backoff: BackoffGate,
    python_check_backoff: BackoffGate,
    go_check_backoff: BackoffGate,

    // Project type detection cache (updated on chpwd only)
    project_root: Option<PathBuf>,
    project_types: ProjectTypeCache,
    rust_runtime_source: Option<String>,
    node_runtime_source: Option<String>,
    python_runtime_source: Option<String>,
    go_runtime_source: Option<String>,

    // Cloud Context
    k8s_context_cache: Option<String>,
    k8s_namespace_cache: Option<String>,
    aws_profile_cache: Option<String>,
    docker_context_cache: Option<String>,
    k8s_check_backoff: BackoffGate,
    docker_check_backoff: BackoffGate,
    last_exit_status: i32,
    last_duration: Option<Duration>,
    /// Lifecycle ownership for async tool probes. External-tool
    /// version/context caches and failure backoffs are scoped to the
    /// current runtime identity (logical PATH generation, snapshot cwd,
    /// exported child environment, prompt variables): an identity change
    /// advances the epoch and invalidates them so the prompt cannot stay
    /// pinned to a previously selected toolchain.
    probe_lifecycle: PromptProbeLifecycle,

    // Module system
    modules: Vec<Box<dyn PromptModule>>,
}

/// Cache for project type detection to avoid repeated file existence checks
#[derive(Debug, Default)]
struct ProjectTypeCache {
    has_cargo_toml: bool,
    has_package_json: bool,
    has_python_project: bool,
    has_go_mod: bool,
}

impl Prompt {
    pub fn new(current_dir: PathBuf, mark: String) -> Prompt {
        let mut prompt = Prompt {
            current_dir: current_dir.clone(),
            mark: mark.clone(),
            github_status: None,
            github_icon: "🐙".to_string(),
            current_git_root: None,
            needs_git_check: true,
            git_root_cache: HashSet::new(),
            git_status_cache: None,
            watcher: None,
            git_sender: None,
            rust_version_cache: None,
            node_version_cache: None,
            python_version_cache: None,
            go_version_cache: None,
            rust_check_backoff: BackoffGate::new(),
            node_check_backoff: BackoffGate::new(),
            python_check_backoff: BackoffGate::new(),
            go_check_backoff: BackoffGate::new(),

            // Project type cache (will be populated in set_current)
            project_root: None,
            project_types: ProjectTypeCache::default(),
            rust_runtime_source: None,
            node_runtime_source: None,
            python_runtime_source: None,
            go_runtime_source: None,

            // Cloud Context
            k8s_context_cache: None,
            k8s_namespace_cache: None,
            aws_profile_cache: None,
            docker_context_cache: None,
            k8s_check_backoff: BackoffGate::new(),
            docker_check_backoff: BackoffGate::new(),
            probe_lifecycle: PromptProbeLifecycle::new(),
            last_exit_status: 0,
            last_duration: None,

            modules: vec![
                Box::new(DirectoryModule::new()),
                Box::new(GitModule::new(BRANCH_MARK.to_string())),
                Box::new(NodeModule::new()),
                Box::new(RustModule::new()),
                Box::new(PythonModule::new()),
                Box::new(GoModule::new()),
                Box::new(KubernetesModule::new()),
                Box::new(AwsModule::new()),
                Box::new(DockerModule::new()),
                Box::new(ExecutionTimeModule::new()),
                Box::new(ExitStatusModule::new()),
                Box::new(TimeModule::new()),
            ],
        };

        // Set Git root during initialization
        prompt.set_current(&current_dir);
        prompt.refresh_project_types();
        prompt
    }

    pub fn set_git_sender(&mut self, sender: UnboundedSender<()>) {
        self.git_sender = Some(sender);
    }

    fn start_watcher(&mut self) {
        if let Some(git_root) = &self.current_git_root
            && let Some(sender) = &self.git_sender
        {
            let sender = sender.clone();
            let mut watcher =
                notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                    Ok(_) => {
                        let _ = sender.send(());
                    }
                    Err(e) => tracing::error!("watch error: {:?}", e),
                })
                .ok();

            if let Some(w) = &mut watcher {
                let git_dir = git_root.join(".git");
                let git_dir = if git_dir.is_file() {
                    if let Ok(content) = std::fs::read_to_string(&git_dir) {
                        if let Some(path) = content.trim().strip_prefix("gitdir: ") {
                            git_root.join(path.trim())
                        } else {
                            self.watcher = watcher;
                            return;
                        }
                    } else {
                        self.watcher = watcher;
                        return;
                    }
                } else {
                    git_dir
                };

                if git_dir.exists() {
                    let head = git_dir.join("HEAD");
                    let index = git_dir.join("index");
                    let refs = git_dir.join("refs");
                    let packed_refs = git_dir.join("packed-refs");

                    if head.exists() {
                        let _ = w.watch(&head, RecursiveMode::NonRecursive);
                    }
                    if index.exists() {
                        let _ = w.watch(&index, RecursiveMode::NonRecursive);
                    }
                    if refs.exists() {
                        let _ = w.watch(&refs, RecursiveMode::Recursive);
                    }
                    if packed_refs.exists() {
                        let _ = w.watch(&packed_refs, RecursiveMode::NonRecursive);
                    }
                }
            }
            self.watcher = watcher;
        }
    }

    // Helper methods (Keep mostly as is)

    pub fn under_git(&self) -> bool {
        if let Some(git_root) = &self.current_git_root {
            self.current_dir.starts_with(git_root)
        } else {
            false
        }
    }

    fn get_git_root_cached_only(&self) -> Option<String> {
        for git_root in &self.git_root_cache {
            if self.current_dir.starts_with(git_root) {
                return Some(git_root.to_string());
            }
        }
        None
    }

    pub fn current_path(&self) -> &Path {
        &self.current_dir
    }

    pub fn set_current(&mut self, path: &Path) {
        let dir_changed = self.current_dir != path;
        self.current_dir = path.to_path_buf();

        // Update project type cache when directory changes
        if dir_changed {
            self.refresh_project_types();

            // Clear version caches when changing directories
            self.rust_version_cache = None;
            self.node_version_cache = None;
            self.python_version_cache = None;
            self.go_version_cache = None;
        }

        let mut root_changed = false;
        if let Some(git_root) = &self.current_git_root {
            if !self.current_dir.starts_with(git_root) {
                root_changed = true;
            }
        } else {
            root_changed = true;
        }

        if root_changed {
            if let Some(root) = self.get_git_root_cached_only() {
                self.current_git_root = Some(PathBuf::from(&root));
                self.git_status_cache = None;
                self.needs_git_check = false;
                self.start_watcher();
            } else {
                self.current_git_root = None;
                self.needs_git_check = true;
                self.watcher = None;
                if let Some(sender) = &self.git_sender {
                    let _ = sender.send(());
                }
            }
        }
    }

    /// Refresh project type cache by checking file existence (called on chpwd only)
    fn refresh_project_types(&mut self) {
        let project = project_context::resolve_project_context(&self.current_dir);
        let root = project.project_root.clone();

        self.project_root = Some(root.clone());
        self.rust_runtime_source = project
            .runtime("rust")
            .map(|runtime| runtime.source.clone());
        self.node_runtime_source = project
            .runtime("node")
            .map(|runtime| runtime.source.clone());
        self.python_runtime_source = project
            .runtime("python")
            .map(|runtime| runtime.source.clone());
        self.go_runtime_source = project.runtime("go").map(|runtime| runtime.source.clone());

        self.project_types = ProjectTypeCache {
            has_cargo_toml: root.join("Cargo.toml").exists(),
            has_package_json: root.join("package.json").exists(),
            has_python_project: root.join("requirements.txt").exists()
                || root.join("pyproject.toml").exists()
                || root.join("Pipfile").exists()
                || root.join(".venv").exists()
                || root.join("venv").exists(),
            has_go_mod: root.join("go.mod").exists(),
        };
    }

    pub fn update_git_root(&mut self, root: Option<PathBuf>) {
        self.current_git_root = root.clone();
        if let Some(r) = root {
            self.git_root_cache.insert(r.to_string_lossy().to_string());
        }
        self.needs_git_check = false;
        self.git_status_cache = None;
    }

    pub fn invalidate_git_cache(&mut self) {
        // We do not clear the cache here to implement Stale-While-Revalidate.
        // The Repl triggers a background check separately.
        // Keeping the old status prevents flickering.
    }

    pub fn trigger_git_check(&self) {
        if let Some(sender) = &self.git_sender {
            let _ = sender.send(());
        }
    }

    pub fn has_git_root(&self) -> bool {
        self.current_git_root.is_some()
    }

    pub fn get_git_status_cached(&self) -> Option<GitStatus> {
        let git_root = self.current_git_root.as_ref()?;
        let cache = self.git_status_cache.as_ref()?;

        if cache.is_valid(git_root) {
            Some(cache.status.clone())
        } else {
            if cache.git_root != *git_root {
                return None;
            }
            Some(cache.status.clone())
        }
    }

    pub fn should_refresh(&self) -> bool {
        let Some(git_root) = &self.current_git_root else {
            return false;
        };

        match &self.git_status_cache {
            Some(cache) => !cache.is_valid(git_root),
            None => true,
        }
    }

    pub fn update_git_status(&mut self, status: Option<GitStatus>) {
        let Some(git_root) = &self.current_git_root else {
            return;
        };

        if let Some(status) = status {
            if let Some(ref mut cache) = self.git_status_cache {
                cache.update(status, git_root.clone());
            } else {
                self.git_status_cache = Some(GitStatusCache::new(status, git_root.clone()));
            }
        }
    }

    /// Synchronously refresh git status for accurate display after command execution.
    /// This blocks but ensures the prompt shows the correct state immediately.
    pub fn refresh_git_status_sync(&mut self) {
        let Some(git_root) = &self.current_git_root else {
            return;
        };

        if let Some(status) = fetch_git_status_sync(git_root) {
            if let Some(ref mut cache) = self.git_status_cache {
                cache.update(status, git_root.clone());
            } else {
                self.git_status_cache = Some(GitStatusCache::new(status, git_root.clone()));
            }
        }
    }

    pub(super) fn get_head_branch(&self) -> Option<String> {
        let git_root = self.current_git_root.as_ref()?;
        let git_dir = git_root.join(".git");

        // Resolve .git file (worktree/submodule)
        let git_dir = if git_dir.is_file() {
            if let Ok(content) = std::fs::read_to_string(&git_dir) {
                {
                    let path = content.trim().strip_prefix("gitdir: ")?;
                    git_root.join(path.trim())
                }
            } else {
                return None;
            }
        } else {
            git_dir
        };

        if !git_dir.exists() {
            return None;
        }

        let head_path = git_dir.join("HEAD");
        if let Ok(head_content) = std::fs::read_to_string(head_path) {
            let content = head_content.trim();
            if let Some(branch_ref) = content.strip_prefix("ref: refs/heads/") {
                return Some(branch_ref.to_string());
            } else {
                if content.len() >= 7 {
                    return Some(content[..7].to_string());
                }
                return Some("DETACHED".to_string());
            }
        }
        None
    }
    pub fn update_rust_version(&mut self, version: Option<String>) {
        self.rust_version_cache = version;
        self.rust_check_backoff.reset();
    }

    pub fn update_node_version(&mut self, version: Option<String>) {
        self.node_version_cache = version;
        self.node_check_backoff.reset();
    }

    pub fn needs_rust_check(&self) -> bool {
        self.rust_version_cache.is_none()
            && self.rust_check_backoff.should_check()
            && self.project_types.has_cargo_toml
    }

    pub fn needs_node_check(&self) -> bool {
        self.node_version_cache.is_none()
            && self.node_check_backoff.should_check()
            && self.project_types.has_package_json
    }

    pub fn update_python_version(&mut self, version: Option<String>) {
        self.python_version_cache = version;
        self.python_check_backoff.reset();
    }

    pub fn update_go_version(&mut self, version: Option<String>) {
        self.go_version_cache = version;
        self.go_check_backoff.reset();
    }

    pub fn needs_python_check(&self) -> bool {
        self.python_version_cache.is_none()
            && self.python_check_backoff.should_check()
            && self.project_types.has_python_project
    }

    pub fn needs_go_check(&self) -> bool {
        self.go_version_cache.is_none()
            && self.go_check_backoff.should_check()
            && self.project_types.has_go_mod
    }

    pub fn update_k8s_info(&mut self, context: Option<String>, namespace: Option<String>) {
        self.k8s_context_cache = context;
        self.k8s_namespace_cache = namespace;
        self.k8s_check_backoff.reset();
    }

    pub fn update_aws_profile(&mut self, profile: Option<String>) {
        self.aws_profile_cache = profile;
    }

    pub fn update_docker_context(&mut self, context: Option<String>) {
        self.docker_context_cache = context;
        self.docker_check_backoff.reset();
    }

    pub(crate) fn should_check_k8s(&self, runtime: &PromptRuntimeSnapshot) -> bool {
        self.k8s_context_cache.is_none()
            && self.k8s_check_backoff.should_check()
            && should_attempt_k8s_context_check_with(runtime)
    }

    pub fn should_check_aws(&self) -> bool {
        self.aws_profile_cache.is_none()
    }

    pub(crate) fn should_check_docker(&self, runtime: &PromptRuntimeSnapshot) -> bool {
        self.docker_context_cache.is_none()
            && self.docker_check_backoff.should_check()
            && should_attempt_docker_context_check_from(runtime)
    }

    /// Observe the runtime identity of a refresh-tick snapshot.
    ///
    /// A changed identity (PATH generation, cwd, exported child env, or
    /// prompt variables) advances the probe epoch and drops
    /// runtime-scoped caches/backoffs so the new runtime is probed
    /// immediately. Re-observing the same identity keeps caches, backoffs,
    /// and the epoch intact.
    pub(crate) fn observe_runtime_identity(
        &mut self,
        identity: PromptRuntimeIdentity,
    ) -> PromptProbeEpoch {
        let observed = self.probe_lifecycle.observe(identity);
        if observed.changed {
            self.invalidate_runtime_scoped_probe_state();
        }
        observed.epoch
    }

    /// Claim the in-flight slot for `probe` at `epoch`. Keeps the
    /// `needs_*`/`should_check_*` cache/backoff/project gates separate:
    /// they decide whether a probe is wanted, the lifecycle decides
    /// whether it may spawn.
    pub(crate) fn try_begin_probe(&mut self, probe: PromptProbe, epoch: PromptProbeEpoch) -> bool {
        self.probe_lifecycle.try_begin(probe, epoch)
    }

    /// Release a probe slot. Returns true only when the completion still
    /// owns the current epoch and may publish success or failure.
    pub(crate) fn finish_probe(&mut self, probe: PromptProbe, epoch: PromptProbeEpoch) -> bool {
        self.probe_lifecycle.finish(probe, epoch)
    }

    /// Drop every runtime-scoped probe cache and reset failure backoffs.
    fn invalidate_runtime_scoped_probe_state(&mut self) {
        self.rust_version_cache = None;
        self.node_version_cache = None;
        self.python_version_cache = None;
        self.go_version_cache = None;
        self.k8s_context_cache = None;
        self.k8s_namespace_cache = None;
        self.aws_profile_cache = None;
        self.docker_context_cache = None;
        self.rust_check_backoff.reset();
        self.node_check_backoff.reset();
        self.python_check_backoff.reset();
        self.go_check_backoff.reset();
        self.k8s_check_backoff.reset();
        self.docker_check_backoff.reset();
    }

    pub fn mark_rust_check_failed(&mut self) {
        self.rust_check_backoff.record_failure();
    }

    pub fn mark_node_check_failed(&mut self) {
        self.node_check_backoff.record_failure();
    }

    pub fn mark_python_check_failed(&mut self) {
        self.python_check_backoff.record_failure();
    }

    pub fn mark_go_check_failed(&mut self) {
        self.go_check_backoff.record_failure();
    }

    pub fn mark_k8s_check_failed(&mut self) {
        self.k8s_check_backoff.record_failure();
    }

    pub fn mark_docker_check_failed(&mut self) {
        self.docker_check_backoff.record_failure();
    }

    pub fn update_status(&mut self, exit_status: i32, duration: Option<Duration>) {
        self.last_exit_status = exit_status;
        self.last_duration = duration;
    }
}
