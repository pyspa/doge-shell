//! `ProcessEnvironmentCapability for Shell`: logical PATH and child environment.
use crate::shell::Shell;

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
}
