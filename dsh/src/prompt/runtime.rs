//! Prompt probe runtime authority: one immutable snapshot per refresh tick.
//!
//! Toolchain and cloud-context probes resolve executables from the logical
//! shell `PATH` and spawn them with the exported child environment, never
//! the process-global state. A refresh tick builds a single
//! [`PromptRuntimeSnapshot`] and shares it across every async probe so they
//! all see the same `PATH`, child environment, cwd, and prompt variables.

use crate::environment::Environment;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The slice of shell runtime state the prompt probes read.
///
/// Snapshotted from `Environment` once per refresh tick
/// ([`PromptEnvironment::from_environment`]) so every probe in the tick sees
/// the same values - and so a shell-level unset stays unset. Nothing here is
/// ever re-read from the process environment afterwards: a stale
/// process-global value must not resurrect a runtime shell variable.
///
/// `home` is the `$HOME` shell variable. When the shell has none, the OS
/// account lookup (`dirs::home_dir()`) still backs the `~/.kube/config`
/// fallback: that is a launch-time fact about the user, not a shell
/// variable.
#[derive(Debug, Clone, Default)]
pub(crate) struct PromptEnvironment {
    pub aws_profile: Option<String>,
    pub aws_default_profile: Option<String>,
    pub docker_context: Option<String>,
    pub kubeconfig: Option<String>,
    pub home: Option<String>,
}

impl PromptEnvironment {
    pub(crate) fn from_environment(environment: &Environment) -> Self {
        Self {
            aws_profile: environment.get_var("AWS_PROFILE"),
            aws_default_profile: environment.get_var("AWS_DEFAULT_PROFILE"),
            docker_context: environment.get_var("DOCKER_CONTEXT"),
            kubeconfig: environment.get_var("KUBECONFIG"),
            home: environment.get_var("HOME"),
        }
    }
}

/// A trimmed, non-blank shell value. Blank counts as unset so an emptied
/// variable falls through to the next source instead of sticking.
pub(crate) fn trimmed_nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The single runtime snapshot for one prompt refresh tick.
///
/// Every async probe in the tick shares the same logical `PATH` entries,
/// exported child environment, snapshot cwd, and prompt variables, so a
/// `PATH` change or `unset` mid-tick cannot make probes disagree with each
/// other or with normal shell command execution.
#[derive(Debug, Clone)]
pub(crate) struct PromptRuntimeSnapshot {
    pub(crate) environment: PromptEnvironment,
    command_search_paths: Vec<PathBuf>,
    child_env: HashMap<String, String>,
    current_dir: PathBuf,
    path_generation: u64,
}

impl PromptRuntimeSnapshot {
    pub(crate) fn from_environment(environment: &Environment, current_dir: PathBuf) -> Self {
        Self {
            environment: PromptEnvironment::from_environment(environment),
            command_search_paths: environment
                .variable_state
                .paths
                .iter()
                .map(PathBuf::from)
                .collect(),
            child_env: environment.child_process_env(),
            current_dir,
            path_generation: environment.completion_state.path_generation,
        }
    }

    pub(crate) fn path_generation(&self) -> u64 {
        self.path_generation
    }

    pub(crate) fn child_env(&self) -> &HashMap<String, String> {
        &self.child_env
    }

    pub(crate) fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    /// Resolve `name` against the snapshot's logical `PATH`, in order.
    ///
    /// Relative entries resolve against the snapshot cwd, so a tick that
    /// started in another directory still finds the same binaries. Only
    /// executable files match; an empty entry behaves like the shell's
    /// cwd fallback. Never consults the process-global `PATH`.
    ///
    /// A name containing `/` bypasses PATH search, like the shell's own
    /// lookup: probes only pass bare tool names, and a pathname must never
    /// be silently reinterpreted against snapshot directories.
    pub(crate) fn resolve_program(&self, name: &str) -> Option<PathBuf> {
        if name.contains('/') {
            return None;
        }
        for entry in &self.command_search_paths {
            let base = if entry.is_absolute() {
                entry.clone()
            } else {
                self.current_dir().join(entry)
            };
            let candidate = base.join(name);
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// Build a subprocess for a prompt probe: the only spawn path probes use.
    ///
    /// The resolved absolute executable path avoids any OS-defined `PATH`
    /// fallback after [`tokio::process::Command::env_clear`], and the child
    /// sees exactly the snapshot's exported environment - a logically unset
    /// variable stays absent even when the process environment still holds
    /// a stale value.
    pub(crate) fn command(&self, program: &str) -> Option<tokio::process::Command> {
        let executable = self.resolve_program(program)?;
        let mut command = tokio::process::Command::new(executable);
        command
            .env_clear()
            .envs(self.child_env())
            .current_dir(self.current_dir());
        Some(command)
    }
}

/// Same executable predicate as shell/task/project resolution: a regular
/// file with any execute bit. `metadata` follows symlinks so a linked
/// toolchain binary stays usable from prompt probes.
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}
