//! Prompt probe runtime authority: one immutable snapshot per refresh tick.
//!
//! Toolchain and cloud-context probes resolve executables from the logical
//! shell `PATH` and spawn them with the exported child environment, never
//! the process-global state. A refresh tick builds a single
//! [`PromptRuntimeSnapshot`] and shares it across every async probe so they
//! all see the same `PATH`, child environment, cwd, and prompt variables.

use crate::environment::Environment;
use dsh_types::process_runtime::CommandRuntimeSnapshot;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

/// Value-compared identity of the runtime a prompt probe belongs to.
///
/// `path_generation` is the authority for `PATH` contents (every logical
/// `PATH` mutation bumps it, even a same-text reassignment, so `PATH=$PATH`
/// stays an explicit rescan boundary). The actual `Vec<PathBuf>` is not
/// duplicated here. `current_dir` covers relative-`PATH` semantics and
/// directory-sensitive lookups (`~/.kube/config` fallback, project
/// location); `child_env` covers shim/toolchain behavior that depends on
/// exported variables (e.g. `RUSTUP_TOOLCHAIN`); `environment` covers
/// prompt-specific logical variables (`AWS_PROFILE`, `DOCKER_CONTEXT`,
/// `KUBECONFIG`, `HOME`) that never move the PATH generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptRuntimeIdentity {
    pub(crate) path_generation: u64,
    pub(crate) current_dir: PathBuf,
    pub(crate) child_env: Arc<HashMap<String, String>>,
    pub(crate) environment: PromptEnvironment,
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
///
/// Executable lookup, environment isolation, and cwd live in the composed
/// [`CommandRuntimeSnapshot`]: the prompt snapshot adds only the
/// prompt-specific [`PromptEnvironment`] and the `PATH` generation that
/// scopes the runtime identity. The prompt identity/epoch discipline
/// (`path_generation`, cwd, child environment, prompt variables) is
/// unchanged.
#[derive(Debug, Clone)]
pub(crate) struct PromptRuntimeSnapshot {
    pub(crate) environment: PromptEnvironment,
    command_runtime: CommandRuntimeSnapshot,
    path_generation: u64,
}

impl PromptRuntimeSnapshot {
    pub(crate) fn from_environment(environment: &Environment, current_dir: PathBuf) -> Self {
        let command_runtime = environment.command_runtime_snapshot(current_dir);
        Self {
            environment: PromptEnvironment::from_environment(environment),
            command_runtime,
            path_generation: environment.completion_state.path_generation,
        }
    }

    /// Value-compared identity of this snapshot's runtime. Shared `Arc`
    /// child environment keeps the clone cheap.
    pub(crate) fn identity(&self) -> PromptRuntimeIdentity {
        PromptRuntimeIdentity {
            path_generation: self.path_generation,
            current_dir: self.command_runtime.current_dir().to_path_buf(),
            child_env: self.command_runtime.child_env_shared(),
            environment: self.environment.clone(),
        }
    }

    pub(crate) fn child_env(&self) -> &HashMap<String, String> {
        self.command_runtime.child_env()
    }

    pub(crate) fn current_dir(&self) -> &Path {
        self.command_runtime.current_dir()
    }

    /// The composed generic runtime: the one immutable `PATH` / child
    /// environment / cwd triple this tick's probes share.
    pub(crate) fn command_runtime(&self) -> &CommandRuntimeSnapshot {
        &self.command_runtime
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
        self.command_runtime.resolve_bare_program(name)
    }

    /// Build a subprocess for a prompt probe: the only spawn path probes use.
    ///
    /// The resolved absolute executable path avoids any OS-defined `PATH`
    /// fallback after [`tokio::process::Command::env_clear`], and the child
    /// sees exactly the snapshot's exported environment - a logically unset
    /// variable stays absent even when the process environment still holds
    /// a stale value. Like [`CommandRuntimeSnapshot::std_command`], the
    /// original program name is kept as `argv[0]` on Unix.
    pub(crate) fn command(&self, program: &str) -> Option<tokio::process::Command> {
        let executable = self.resolve_program(program)?;
        let mut command = tokio::process::Command::new(executable);
        command
            .env_clear()
            .envs(self.child_env())
            .current_dir(self.current_dir());
        // `arg0` is an inherent tokio `Command` method on Unix (no trait
        // import needed): keep the original program name as argv[0].
        #[cfg(unix)]
        {
            command.arg0(program);
        }
        Some(command)
    }
}
