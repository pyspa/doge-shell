//! Project provider runtime snapshot (private to `project`).
//!
//! Mise executable lookup uses only the logical shell command search paths,
//! and mise subprocesses receive only the shell's exported child environment.
//! One snapshot is shared across trust / missing-tool / env probes within a
//! single `pm status` or `pm activate` operation.

use super::ShellProxy;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub(super) struct ProjectProviderRuntime {
    command_search_paths: Vec<PathBuf>,
    child_env: HashMap<String, String>,
}

impl ProjectProviderRuntime {
    pub(super) fn new(
        command_search_paths: Vec<PathBuf>,
        child_env: HashMap<String, String>,
    ) -> Self {
        Self {
            command_search_paths,
            child_env,
        }
    }

    pub(super) fn from_proxy(proxy: &dyn ShellProxy) -> Self {
        Self::new(
            proxy.command_search_paths(),
            proxy.child_process_environment(),
        )
    }

    pub(super) fn resolve_program(&self, name: &str) -> Option<PathBuf> {
        self.command_search_paths
            .iter()
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable_file(candidate))
    }

    pub(super) fn child_env(&self) -> &HashMap<String, String> {
        &self.child_env
    }

    pub(super) fn configure_command<'a>(&'a self, command: &'a mut Command) -> &'a mut Command {
        command.env_clear().envs(self.child_env())
    }
}

impl std::fmt::Debug for ProjectProviderRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print environment values: they may hold secrets.
        f.debug_struct("ProjectProviderRuntime")
            .field("command_search_paths_len", &self.command_search_paths.len())
            .field("child_env_len", &self.child_env.len())
            .finish()
    }
}

fn is_executable_file(path: &std::path::Path) -> bool {
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
        let _ = metadata;
        false
    }
}
