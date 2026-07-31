use super::super::integrated::EnhancedCandidate;
use dsh_builtin::task;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileMetadataSignature {
    pub exists: bool,
    pub modified: Option<SystemTime>,
    pub len: u64,
}

#[derive(Debug, Clone)]
pub(super) struct TaskCacheEntry {
    pub signature: Vec<FileMetadataSignature>,
    pub tasks: Vec<task::TaskInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct TaskCacheKey {
    pub project_root: PathBuf,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ComposeCacheEntry {
    pub signature: FileMetadataSignature,
    pub services: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct CommandValueCacheEntry {
    pub values: Vec<String>,
    pub cached_at: Instant,
    pub last_load_duration: Option<Duration>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct CommandValueErrorEntry {
    pub recorded_at: Instant,
    pub last_load_duration: Duration,
    pub error: String,
}

#[derive(Debug, Clone)]
pub(super) struct ProjectRootCacheEntry {
    pub project_root: PathBuf,
    pub cached_at: Instant,
}

#[derive(Debug, Clone)]
pub(super) struct ExternalCompletionCacheEntry {
    pub candidates: Vec<EnhancedCandidate>,
    pub cached_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum DynamicCommandCacheKind {
    GitBranch,
    GitRemote,
    GitWorktree,
    KubectlContext,
    KubectlNamespace,
    CommandValue { command: String, value_kind: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct DynamicCommandCacheKey {
    pub kind: DynamicCommandCacheKind,
    pub scope_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ExternalCompletionCacheKey {
    pub command_template: String,
    pub current_dir: PathBuf,
    pub input: String,
    pub cursor_pos: usize,
    pub command: String,
    pub current_token: String,
    pub subcommand_path: String,
}

#[derive(Debug, Default)]
pub(super) struct ProjectDynamicCache {
    pub tasks: HashMap<TaskCacheKey, TaskCacheEntry>,
    pub compose_services: HashMap<PathBuf, ComposeCacheEntry>,
    pub commands: HashMap<DynamicCommandCacheKey, CommandValueCacheEntry>,
    pub command_errors: HashMap<DynamicCommandCacheKey, CommandValueErrorEntry>,
    pub command_pending: HashSet<DynamicCommandCacheKey>,
    pub external: HashMap<ExternalCompletionCacheKey, ExternalCompletionCacheEntry>,
    pub external_pending: HashSet<ExternalCompletionCacheKey>,
    pub external_pruned_total: usize,
    pub project_roots: HashMap<PathBuf, ProjectRootCacheEntry>,
}
