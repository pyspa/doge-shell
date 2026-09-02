use crate::ShellProxy;
use crate::shell_capabilities::{AgentCommandPolicy, AgentCommandVerdict, ApprovalDecision};
use anyhow::Result;
use dsh_types::{Context, mcp::McpServerConfig};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// Shared fail-closed shell double for builtin tests.
///
/// State-changing operations that can report failure reject work unless the
/// corresponding `allow_*` switch is enabled. Individual tests can opt in and
/// inspect the recorded operation without reimplementing the entire legacy
/// proxy surface.
#[derive(Debug)]
pub(crate) struct TestShellProxy {
    pub current_dir: PathBuf,
    pub changed_to: Option<String>,
    pub allow_changepwd: bool,
    pub allow_dispatch: bool,
    pub dispatched: Vec<(String, Vec<String>)>,
    pub confirm_result: bool,
    pub confirm_calls: usize,
    pub confirm_counter: Option<Arc<AtomicUsize>>,
    pub execute_allowlist: Vec<String>,
    /// What `evaluate_agent_command` answers. Fail-closed like the rest of this
    /// double: without an opt-in every command needs confirmation, and
    /// `confirm_result` then decides.
    pub agent_verdict: AgentCommandVerdict,
    /// What `evaluate_agent_tool` answers. Separate from `agent_verdict` so a
    /// test can pin the MCP policy without also unlocking `execute`.
    pub agent_tool_verdict: AgentCommandVerdict,
    pub agent_session_allowlist: Vec<String>,
    /// The MCP manager the chat runtime is handed. Shared, so a test can seed a
    /// binding the way the shell would.
    pub mcp_manager: Arc<RwLock<crate::chatgpt::McpManager>>,
    /// Overrides `confirm_result` so a test can exercise the "always" answer.
    pub approval_decision: Option<ApprovalDecision>,
    pub vars: HashMap<String, String>,
    pub aliases: HashMap<String, String>,
    pub abbrs: HashMap<String, String>,
    pub exported: HashMap<String, String>,
}

impl Default for TestShellProxy {
    fn default() -> Self {
        Self {
            current_dir: PathBuf::from("/"),
            changed_to: None,
            allow_changepwd: false,
            allow_dispatch: false,
            dispatched: Vec::new(),
            confirm_result: false,
            confirm_calls: 0,
            confirm_counter: None,
            execute_allowlist: Vec::new(),
            agent_verdict: AgentCommandVerdict::Confirm(
                "test policy requires confirmation".to_string(),
            ),
            agent_tool_verdict: AgentCommandVerdict::Confirm(
                "test policy requires confirmation".to_string(),
            ),
            agent_session_allowlist: Vec::new(),
            mcp_manager: Arc::new(RwLock::new(crate::chatgpt::McpManager::default())),
            approval_decision: None,
            vars: HashMap::new(),
            aliases: HashMap::new(),
            abbrs: HashMap::new(),
            exported: HashMap::new(),
        }
    }
}

impl ShellProxy for TestShellProxy {
    fn exit_shell(&mut self) {}

    fn get_github_status(&self) -> (usize, usize, usize) {
        (0, 0, 0)
    }

    fn get_git_branch(&self) -> Option<String> {
        None
    }

    fn get_job_count(&self) -> usize {
        0
    }

    fn dispatch(&mut self, _ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()> {
        if !self.allow_dispatch {
            return Err(anyhow::anyhow!("dispatch not configured"));
        }
        self.dispatched.push((cmd.to_string(), argv));
        Ok(())
    }

    fn save_path_history(&mut self, _path: &str) {}

    fn changepwd(&mut self, path: &str) -> Result<()> {
        if !self.allow_changepwd {
            return Err(anyhow::anyhow!("changepwd not configured"));
        }
        self.current_dir = PathBuf::from(path);
        self.changed_to = Some(path.to_string());
        Ok(())
    }

    fn insert_path(&mut self, _index: usize, _path: &str) {}

    fn get_var(&mut self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }

    fn set_var(&mut self, key: String, value: String) {
        self.vars.insert(key, value);
    }

    fn set_env_var(&mut self, key: String, value: String) {
        self.exported.insert(key, value);
    }

    fn unset_env_var(&mut self, key: &str) {
        self.exported.remove(key);
    }

    fn get_alias(&mut self, name: &str) -> Option<String> {
        self.aliases.get(name).cloned()
    }

    fn set_alias(&mut self, name: String, command: String) {
        self.aliases.insert(name, command);
    }

    fn list_aliases(&mut self) -> HashMap<String, String> {
        self.aliases.clone()
    }

    fn add_abbr(&mut self, name: String, expansion: String) {
        self.abbrs.insert(name, expansion);
    }

    fn remove_abbr(&mut self, name: &str) -> bool {
        self.abbrs.remove(name).is_some()
    }

    fn list_abbrs(&self) -> Vec<(String, String)> {
        self.abbrs
            .iter()
            .map(|(name, expansion)| (name.clone(), expansion.clone()))
            .collect()
    }

    fn get_abbr(&self, name: &str) -> Option<String> {
        self.abbrs.get(name).cloned()
    }

    fn list_mcp_servers(&mut self) -> Vec<McpServerConfig> {
        Vec::new()
    }

    fn list_execute_allowlist(&mut self) -> Vec<String> {
        self.execute_allowlist.clone()
    }

    fn list_exported_vars(&self) -> Vec<(String, String)> {
        self.exported
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    fn export_var(&mut self, key: &str) -> bool {
        let Some(value) = self.vars.get(key).cloned() else {
            return false;
        };
        self.exported.insert(key.to_string(), value);
        true
    }

    fn set_and_export_var(&mut self, key: String, value: String) {
        self.vars.insert(key.clone(), value.clone());
        self.exported.insert(key, value);
    }

    fn get_current_dir(&self) -> Result<PathBuf> {
        Ok(self.current_dir.clone())
    }

    fn get_lisp_var(&self, _key: &str) -> Option<String> {
        None
    }

    fn confirm_action(&mut self, _message: &str) -> Result<bool> {
        self.confirm_calls += 1;
        if let Some(counter) = &self.confirm_counter {
            counter.fetch_add(1, Ordering::SeqCst);
        }
        Ok(self.confirm_result)
    }
}

impl AgentCommandPolicy for TestShellProxy {
    fn evaluate_agent_command(&mut self, _command: &str) -> AgentCommandVerdict {
        self.agent_verdict.clone()
    }

    fn request_agent_approval(&mut self, _message: &str) -> Result<ApprovalDecision> {
        self.confirm_calls += 1;
        if let Some(counter) = &self.confirm_counter {
            counter.fetch_add(1, Ordering::SeqCst);
        }
        Ok(self.approval_decision.unwrap_or({
            if self.confirm_result {
                ApprovalDecision::Allow
            } else {
                ApprovalDecision::Deny
            }
        }))
    }

    fn remember_agent_approval(&mut self, command: &str) {
        self.agent_session_allowlist.push(command.to_string());
    }

    fn agent_session_approvals(&mut self) -> Vec<String> {
        self.agent_session_allowlist.clone()
    }

    fn agent_allowlist(&mut self) -> Vec<String> {
        self.execute_allowlist.clone()
    }

    fn evaluate_agent_tool(&mut self, _name: &str, _arguments: &str) -> AgentCommandVerdict {
        self.agent_tool_verdict.clone()
    }

    fn agent_tool_approval_entry(&mut self, name: &str, _arguments: &str) -> String {
        format!("mcp:{name}")
    }

    fn agent_mcp_manager(&mut self) -> Arc<RwLock<crate::chatgpt::McpManager>> {
        Arc::clone(&self.mcp_manager)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutations_fail_closed_until_enabled() {
        let mut proxy = TestShellProxy::default();
        let pid = nix::unistd::getpid();
        let ctx = Context::new_safe(pid, pid, false);

        assert!(proxy.changepwd("/tmp").is_err());
        assert!(proxy.dispatch(&ctx, "sh", Vec::new()).is_err());
        assert!(proxy.changed_to.is_none());
        assert!(proxy.dispatched.is_empty());
    }
}
