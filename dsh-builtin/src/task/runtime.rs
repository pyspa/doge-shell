//! Task discovery runtime snapshot: logical PATH authority, exported child
//! environment, and the canonical cache signature.
//!
//! Runtime task discovery resolves provider executables through
//! `Environment.variable_state.paths` and spawns providers with only
//! `Environment::child_process_env()`. The process-global environment is
//! never re-read here, so a logically unset variable stays unset.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

/// Snapshot of what task discovery may observe: where to look for provider
/// executables and what environment to hand to provider subprocesses.
#[derive(Clone)]
pub struct TaskDiscoveryRuntime {
    command_search_paths: Arc<[PathBuf]>,
    child_env: Arc<BTreeMap<String, String>>,
    cache_scope: u64,
}

impl std::fmt::Debug for TaskDiscoveryRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print environment values: they may hold secrets. The
        // fingerprint is what caches and diagnostics need.
        f.debug_struct("TaskDiscoveryRuntime")
            .field("command_search_paths", &self.command_search_paths)
            .field("child_env_len", &self.child_env.len())
            .field("cache_scope", &self.cache_scope)
            .finish()
    }
}

impl TaskDiscoveryRuntime {
    /// Build a snapshot from one shell read-lock section. The caller clones
    /// `variable_state.paths` and `child_process_env()` together, then drops
    /// the lock before any filesystem scan or subprocess runs.
    pub fn new(command_search_paths: Vec<PathBuf>, child_env: HashMap<String, String>) -> Self {
        let child_env: BTreeMap<String, String> = child_env.into_iter().collect();
        let cache_scope = fingerprint_runtime(&command_search_paths, &child_env);
        Self {
            command_search_paths: command_search_paths.into(),
            child_env: Arc::new(child_env),
            cache_scope,
        }
    }

    /// Logical shell PATH snapshot: the only authority for executable lookup.
    pub fn command_search_paths(&self) -> &[PathBuf] {
        &self.command_search_paths
    }

    /// Exported shell variables only, in a fixed order for hashing and envs().
    pub fn child_env(&self) -> &BTreeMap<String, String> {
        &self.child_env
    }

    /// Fingerprint of the runtime snapshot; part of the cache signature.
    pub fn cache_scope(&self) -> u64 {
        self.cache_scope
    }

    /// Resolve `name` through the logical shell PATH, requiring the
    /// executable bit. Never consults `std::env::PATH`.
    pub fn resolve_program(&self, name: &str) -> Option<PathBuf> {
        self.command_search_paths
            .iter()
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable_file(candidate))
    }

    /// Prefer `<root>/node_modules/.bin/<name>`, then the logical shell PATH.
    pub fn resolve_project_program(&self, root: &Path, name: &str) -> Option<PathBuf> {
        let local = root.join("node_modules").join(".bin").join(name);
        if is_executable_file(&local) {
            return Some(local);
        }
        self.resolve_program(name)
    }
}

/// Whether `path` names an executable file.
///
/// Follows symlinks via `metadata` so a linked provider still resolves.
/// Linux and macOS share this executable-bit check: any of the owner, group,
/// or other execute bits qualifies.
pub fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        // doge-shell targets Linux and macOS only; both are unix.
        // Keep the non-unix arm fail-closed rather than executable-by-default.
        let _ = metadata;
        false
    }
}

fn fingerprint_runtime(paths: &[PathBuf], env: &BTreeMap<String, String>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "dogesh-task-runtime-v1".hash(&mut hasher);
    for path in paths {
        path.hash(&mut hasher);
    }
    for (key, value) in env {
        key.hash(&mut hasher);
        value.hash(&mut hasher);
    }
    hasher.finish()
}

/// Canonical task discovery signature: project input metadata plus the
/// runtime discovery identity. Completion and builtin caches share this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskDiscoverySignature(u64);

/// Top-level project files that can change task discovery results.
const TOP_LEVEL_MARKERS: &[&str] = &[
    "mise.toml",
    ".mise.toml",
    "package.json",
    "bun.lockb",
    "pnpm-lock.yaml",
    "yarn.lock",
    "Cargo.toml",
    "Makefile",
    "makefile",
    "Justfile",
    "justfile",
    ".justfile",
    "Taskfile.yml",
    "Taskfile.yaml",
    "turbo.json",
    "nx.json",
    "workspace.json",
    "angular.json",
    "project.json",
    "deno.json",
    "deno.jsonc",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "gradle.properties",
    "gradlew",
];

const DESCENDANT_MARKERS: &[&str] = &["project.json", "package.json", "mise.toml", ".mise.toml"];

const SKIPPED_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", "build"];

/// Compute the canonical signature without running any provider.
pub fn discovery_signature(
    project_root: &Path,
    sources: Option<&[&str]>,
    runtime: &TaskDiscoveryRuntime,
) -> TaskDiscoverySignature {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "dogesh-task-discovery-v1".hash(&mut hasher);
    runtime.cache_scope.hash(&mut hasher);
    match sources {
        Some(sources) => {
            let mut normalized: Vec<&str> = sources.to_vec();
            normalized.sort_unstable();
            normalized.dedup();
            for source in normalized {
                source.hash(&mut hasher);
            }
        }
        None => "all-sources".hash(&mut hasher),
    }
    for name in TOP_LEVEL_MARKERS {
        name.hash(&mut hasher);
        hash_marker_metadata(&project_root.join(name), &mut hasher);
    }
    // Descendant markers feed the static nx scan, which only runs for
    // unfiltered or nx-scoped discovery. Other scopes skip the walk so a
    // nested `project.json` change does not invalidate JS-only completions.
    if sources.is_none_or(|sources| sources.contains(&"nx")) {
        hash_descendant_markers(project_root, &mut hasher);
    }
    hash_provider_identities(project_root, sources, runtime, &mut hasher);
    TaskDiscoverySignature(hasher.finish())
}

fn hash_marker_metadata(path: &Path, hasher: &mut impl Hasher) {
    path.hash(hasher);
    match fs::metadata(path) {
        Ok(metadata) => {
            true.hash(hasher);
            metadata.len().hash(hasher);
            metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .hash(hasher);
            is_executable_file(path).hash(hasher);
        }
        Err(_) => false.hash(hasher),
    }
}

fn hash_descendant_markers(root: &Path, hasher: &mut impl Hasher) {
    hash_descendant_dir(root, root, 0, 4, hasher);
}

fn hash_descendant_dir(
    root: &Path,
    directory: &Path,
    depth: usize,
    max_depth: usize,
    hasher: &mut impl Hasher,
) {
    if depth >= max_depth {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skip = entry
                .file_name()
                .to_str()
                .is_some_and(|name| SKIPPED_DIRS.contains(&name));
            if skip {
                continue;
            }
            hash_descendant_dir(root, &path, depth + 1, max_depth, hasher);
        } else if depth > 0
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| DESCENDANT_MARKERS.contains(&name))
        {
            path.strip_prefix(root).unwrap_or(&path).hash(hasher);
            hash_marker_metadata(&path, hasher);
        }
    }
}

fn has_mise_marker(root: &Path) -> bool {
    root.join("mise.toml").exists() || root.join(".mise.toml").exists()
}

fn has_nx_marker(root: &Path) -> bool {
    ["nx.json", "workspace.json", "angular.json", "project.json"]
        .iter()
        .any(|name| root.join(name).exists())
}

fn has_gradle_project(root: &Path) -> bool {
    [
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "gradle.properties",
        "gradlew",
    ]
    .iter()
    .any(|name| root.join(name).exists())
}

fn has_justfile(root: &Path) -> bool {
    ["Justfile", "justfile", ".justfile"]
        .iter()
        .any(|name| root.join(name).exists())
}

fn hash_provider_identities(
    root: &Path,
    sources: Option<&[&str]>,
    runtime: &TaskDiscoveryRuntime,
    hasher: &mut impl Hasher,
) {
    // Only providers that can run under this filter contribute: an
    // npm-scoped lookup never executes `just`, so installing `just` must
    // not invalidate its cache or cost a PATH resolution here.
    if source_enabled(sources, "mise") && has_mise_marker(root) {
        "provider:mise".hash(hasher);
        hash_resolved_identity(&runtime.resolve_program("mise"), hasher);
    }
    if source_enabled(sources, "nx") && has_nx_marker(root) {
        "provider:nx".hash(hasher);
        hash_resolved_identity(&runtime.resolve_project_program(root, "nx"), hasher);
    }
    if source_enabled(sources, "turbo") && root.join("turbo.json").exists() {
        "provider:turbo".hash(hasher);
        hash_resolved_identity(&runtime.resolve_project_program(root, "turbo"), hasher);
    }
    if source_enabled(sources, "gradle") && has_gradle_project(root) {
        "provider:gradle".hash(hasher);
        let wrapper = root.join("gradlew");
        if is_executable_file(&wrapper) {
            "gradlew-local".hash(hasher);
            hash_marker_metadata(&wrapper, hasher);
        } else {
            "gradle-global".hash(hasher);
            hash_resolved_identity(&runtime.resolve_program("gradle"), hasher);
        }
    }
    if source_enabled(sources, "make")
        && (root.join("Makefile").exists() || root.join("makefile").exists())
    {
        "provider:make".hash(hasher);
        hash_resolved_identity(&runtime.resolve_program("make"), hasher);
    }
    if source_enabled(sources, "just") && has_justfile(root) {
        "provider:just".hash(hasher);
        hash_resolved_identity(&runtime.resolve_program("just"), hasher);
    }
}

fn source_enabled(sources: Option<&[&str]>, source: &str) -> bool {
    sources.is_none_or(|sources| sources.contains(&source))
}

fn hash_resolved_identity(resolved: &Option<PathBuf>, hasher: &mut impl Hasher) {
    match resolved {
        Some(path) => {
            true.hash(hasher);
            path.hash(hasher);
            hash_marker_metadata(path, hasher);
        }
        None => false.hash(hasher),
    }
}
