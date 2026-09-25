//! `ProcessEnvironmentCapability for Shell`: logical PATH and child environment.
use crate::environment::Environment;
use crate::shell::Shell;
use anyhow::Context as _;

/// Snapshot the process cwd, then build the runtime under the Environment
/// read lock. The lock is released before any child runs: the snapshot owns
/// everything the spawn needs. Shared by the capability impl below and the
/// `ShellProxy` methods that spawn through the logical runtime.
pub(crate) fn snapshot_current(
    env: &Environment,
) -> anyhow::Result<dsh_types::process_runtime::CommandRuntimeSnapshot> {
    let current_dir = std::env::current_dir().context("failed to get current directory")?;
    Ok(env.command_runtime_snapshot(current_dir))
}

impl dsh_builtin::shell_capabilities::ProcessEnvironmentCapability for Shell {
    fn child_process_environment(&self) -> std::collections::HashMap<String, String> {
        self.environment.read().child_process_env()
    }

    fn command_search_paths(&self) -> Vec<std::path::PathBuf> {
        let env = self.environment.read();
        env.variable_state
            .paths
            .iter()
            .map(std::path::PathBuf::from)
            .collect()
    }

    fn command_runtime_snapshot(
        &self,
    ) -> anyhow::Result<dsh_types::process_runtime::CommandRuntimeSnapshot> {
        snapshot_current(&self.environment.read())
    }
}
