use crate::shell_capabilities::{AgentCommandPolicy, AgentCommandVerdict, ApprovalDecision};
use crate::{ProxyFuture, ShellProxy};
use anyhow::Result;
use dsh_types::{
    Context, command_block::CommandBlock, mcp::McpServerConfig, output_history::OutputEntry,
    snippet::Snippet,
};
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
pub(crate) struct TestShellProxy {
    pub current_dir: PathBuf,
    pub changed_to: Option<String>,
    pub allow_changepwd: bool,
    pub allow_dispatch: bool,
    pub dispatched: Vec<(String, Vec<String>)>,
    /// Every `dispatch` call, recorded before `dispatch_error`/`allow_dispatch`
    /// decide the outcome - unlike `dispatched`, which only ever holds calls
    /// that returned `Ok`. Lets a test assert the caller passed the right
    /// command/argv even when it configured `dispatch()` to fail.
    pub dispatch_attempts: Vec<(String, Vec<String>)>,
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
    /// Present when the test is exercising the persistent-task path, where
    /// nothing may prompt and every refusal has to be an error.
    pub agent_runtime: Option<Arc<parking_lot::Mutex<crate::agent::AgentRuntime>>>,
    pub vars: HashMap<String, String>,
    pub aliases: HashMap<String, String>,
    pub abbrs: HashMap<String, String>,
    pub exported: HashMap<String, String>,
    /// Number of `set_env_var` calls, independent of `exported`'s contents -
    /// some tests assert "nothing was exported" by call count rather than by
    /// diffing the map.
    pub set_env_calls: usize,
    /// Number of `insert_path` calls.
    pub insert_path_calls: usize,
    pub mcp_servers: Vec<McpServerConfig>,
    pub direnv_allowed: bool,
    pub output_history: Vec<OutputEntry>,
    pub command_blocks: Vec<CommandBlock>,
    pub snippets: HashMap<String, Snippet>,
    pub bookmarks: HashMap<String, (String, i64)>,
    pub last_command: Option<String>,
    /// `open_editor`'s canned reply. `None` fails closed (the same fail-safe
    /// shape `ShellProxy`'s methods used to default to, back when the trait
    /// had default bodies at all).
    pub open_editor_response: Option<String>,
    pub open_editor_calls: Vec<(String, String)>,
    pub capture_command_response: Option<(i32, String, String)>,
    /// `ask_ai_async`'s canned reply. `None` fails closed.
    pub ai_response: Option<String>,
    pub requested_eval: Vec<String>,
    /// When set, `request_eval_command` fails with this message instead of
    /// recording into `requested_eval`.
    pub request_eval_error: Option<String>,
    /// Overrides `dispatch` to fail with this message regardless of
    /// `allow_dispatch` - for exercising a caller's error handling rather
    /// than the dispatch-not-configured default.
    pub dispatch_error: Option<String>,
    /// Opt-in: also mirror `set_env_var`/`unset_env_var` into the real
    /// process environment. Off by default so ordinary tests cannot leak
    /// state into each other; a test that specifically exercises "does this
    /// builtin export a real environment variable" turns it on.
    ///
    /// This does not by itself serialize against other tests that also
    /// mutate `std::env` - the crate has one shared lock for that,
    /// `chatgpt::tool::execute::tests::env_lock()`. A test that sets this
    /// flag must take that lock (`let _lock = crate::chatgpt::tool::execute
    /// ::tests::env_lock();`), the same as every other env-touching test in
    /// this crate, rather than declare a lock of its own - two independent
    /// mutexes guarding the same process-global `std::env` is exactly the
    /// race this flag exists to avoid.
    pub mutate_real_env: bool,
    /// Every `cron_tool_call` request, in order.
    pub cron_tool_calls: Vec<dsh_types::cron::tool::CronToolRequest>,
    /// `cron_tool_call`'s canned reply. `None` answers with an empty object.
    pub cron_tool_response: Option<serde_json::Value>,
    /// When set, `cron_tool_call` fails with this message instead of
    /// returning `cron_tool_response`.
    pub cron_tool_error: Option<String>,
}

impl Default for TestShellProxy {
    fn default() -> Self {
        Self {
            current_dir: PathBuf::from("/"),
            changed_to: None,
            allow_changepwd: false,
            allow_dispatch: false,
            dispatched: Vec::new(),
            dispatch_attempts: Vec::new(),
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
            agent_runtime: None,
            vars: HashMap::new(),
            aliases: HashMap::new(),
            abbrs: HashMap::new(),
            exported: HashMap::new(),
            set_env_calls: 0,
            insert_path_calls: 0,
            mcp_servers: Vec::new(),
            direnv_allowed: false,
            output_history: Vec::new(),
            command_blocks: Vec::new(),
            snippets: HashMap::new(),
            bookmarks: HashMap::new(),
            last_command: None,
            open_editor_response: None,
            open_editor_calls: Vec::new(),
            capture_command_response: None,
            ai_response: None,
            requested_eval: Vec::new(),
            request_eval_error: None,
            dispatch_error: None,
            mutate_real_env: false,
            cron_tool_calls: Vec::new(),
            cron_tool_response: None,
            cron_tool_error: None,
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
        self.dispatch_attempts.push((cmd.to_string(), argv.clone()));
        if let Some(message) = &self.dispatch_error {
            return Err(anyhow::anyhow!(message.clone()));
        }
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

    fn insert_path(&mut self, _index: usize, _path: &str) {
        self.insert_path_calls += 1;
    }

    fn get_var(&mut self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }

    fn set_var(&mut self, key: String, value: String) {
        self.vars.insert(key, value);
    }

    fn set_env_var(&mut self, key: String, value: String) {
        self.set_env_calls += 1;
        if self.mutate_real_env {
            // SAFETY: opt-in only (`mutate_real_env`); callers that turn this
            // on take responsibility for serializing against other tests
            // that touch the same environment variables.
            unsafe { std::env::set_var(&key, &value) };
        }
        self.exported.insert(key, value);
    }

    fn is_direnv_allowed(&self, _path: &Path) -> bool {
        self.direnv_allowed
    }

    fn unset_env_var(&mut self, key: &str) {
        if self.mutate_real_env {
            // SAFETY: see `set_env_var` above.
            unsafe { std::env::remove_var(key) };
        }
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
        self.mcp_servers.clone()
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

    fn get_full_output_history(&self) -> Vec<OutputEntry> {
        self.output_history.clone()
    }

    fn clear_output_history(&mut self) -> usize {
        let removed = self.output_history.len();
        self.output_history.clear();
        removed
    }

    fn get_command_blocks(&self) -> Vec<CommandBlock> {
        self.command_blocks.clone()
    }

    fn clear_command_blocks(&mut self) -> usize {
        let removed = self.command_blocks.len();
        self.command_blocks.clear();
        removed
    }

    fn request_eval_command(&mut self, command: String) -> Result<()> {
        if let Some(message) = &self.request_eval_error {
            return Err(anyhow::anyhow!(message.clone()));
        }
        self.requested_eval.push(command);
        Ok(())
    }

    fn capture_command(&mut self, _ctx: &Context, _cmd: &str) -> Result<(i32, String, String)> {
        self.capture_command_response
            .clone()
            .ok_or_else(|| anyhow::anyhow!("capture_command not configured"))
    }

    fn open_editor(&mut self, content: &str, extension: &str) -> Result<String> {
        self.open_editor_calls
            .push((content.to_string(), extension.to_string()));
        self.open_editor_response
            .clone()
            .ok_or_else(|| anyhow::anyhow!("open_editor not configured"))
    }

    fn ask_ai_async<'a>(
        &'a mut self,
        _messages: Vec<serde_json::Value>,
    ) -> ProxyFuture<'a, String> {
        let response = self.ai_response.clone();
        Box::pin(
            async move { response.ok_or_else(|| anyhow::anyhow!("no ai response configured")) },
        )
    }

    // Snippet management.
    fn add_snippet(&mut self, name: String, command: String, description: Option<String>) -> bool {
        self.snippets.insert(
            name.clone(),
            Snippet {
                id: 0,
                name,
                command,
                description,
                tags: None,
                created_at: 0,
                last_used: None,
                use_count: 0,
            },
        );
        true
    }

    fn remove_snippet(&mut self, name: &str) -> bool {
        self.snippets.remove(name).is_some()
    }

    fn list_snippets(&self) -> Vec<Snippet> {
        self.snippets.values().cloned().collect()
    }

    fn get_snippet(&self, name: &str) -> Option<Snippet> {
        self.snippets.get(name).cloned()
    }

    fn update_snippet(&mut self, name: &str, command: &str, description: Option<&str>) -> bool {
        let Some(snippet) = self.snippets.get_mut(name) else {
            return false;
        };
        snippet.command = command.to_string();
        snippet.description = description.map(str::to_string);
        true
    }

    fn record_snippet_use(&mut self, name: &str) {
        if let Some(snippet) = self.snippets.get_mut(name) {
            snippet.use_count += 1;
        }
    }

    // Bookmark management.
    fn add_bookmark(&mut self, name: String, command: String) -> bool {
        self.bookmarks.insert(name, (command, 0));
        true
    }

    fn remove_bookmark(&mut self, name: &str) -> bool {
        self.bookmarks.remove(name).is_some()
    }

    fn list_bookmarks(&self) -> Vec<(String, String, i64)> {
        self.bookmarks
            .iter()
            .map(|(name, (command, use_count))| (name.clone(), command.clone(), *use_count))
            .collect()
    }

    fn get_bookmark(&self, name: &str) -> Option<(String, i64)> {
        self.bookmarks.get(name).cloned()
    }

    fn record_bookmark_use(&mut self, name: &str) {
        if let Some(bookmark) = self.bookmarks.get_mut(name) {
            bookmark.1 += 1;
        }
    }

    fn get_last_command(&self) -> Option<String> {
        self.last_command.clone()
    }

    fn add_dir_alias(&mut self, _name: String, _path: String) -> bool {
        false
    }

    fn remove_dir_alias(&mut self, _name: &str) -> bool {
        false
    }

    fn list_dir_aliases(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    fn get_dir_alias(&self, _name: &str) -> Option<String> {
        None
    }

    fn command_history_len(&self) -> Option<usize> {
        None
    }

    fn completion_diagnostics(&self) -> Vec<String> {
        Vec::new()
    }

    fn dir_stack(&self) -> Vec<String> {
        Vec::new()
    }

    fn dir_stack_set(&mut self, _stack: Vec<String>) {}

    fn dispatch_core_action(
        &mut self,
        ctx: &Context,
        action: crate::CoreShellAction,
        argv: Vec<String>,
    ) -> Result<()> {
        self.dispatch(ctx, action.command_name(), argv)
    }

    fn executable_cache_len(&self) -> Option<usize> {
        None
    }

    fn generate_command_completion_async<'a>(
        &'a mut self,
        _command_name: &'a str,
        _help_text: &'a str,
    ) -> ProxyFuture<'a, String> {
        Box::pin(async move {
            Err(anyhow::anyhow!(
                "generate_command_completion_async not implemented"
            ))
        })
    }

    fn is_canceled(&self) -> bool {
        false
    }

    fn latency_probe_lines(&self, _iterations: usize) -> Vec<String> {
        Vec::new()
    }

    fn run_hook(&mut self, _hook_name: &str, _args: Vec<String>) -> Result<()> {
        Err(anyhow::anyhow!("run_hook not implemented"))
    }

    fn safety_level(&mut self) -> dsh_types::safety_policy::SafetyLevel {
        dsh_types::safety_policy::SafetyLevel::from_env_value(self.get_var("SAFETY_LEVEL"))
    }

    fn save_output_history(&mut self, entry: OutputEntry) {
        self.output_history.push(entry);
    }

    fn select_item(&mut self, _items: Vec<String>) -> Result<Option<String>> {
        Err(anyhow::anyhow!("select_item not implemented"))
    }
}

impl AgentCommandPolicy for TestShellProxy {
    fn agent_runtime(&self) -> Option<Arc<parking_lot::Mutex<crate::agent::AgentRuntime>>> {
        self.agent_runtime.clone()
    }

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

impl crate::shell_capabilities::AiJsonRequest for TestShellProxy {
    fn ask_ai_json_async<'a>(
        &'a mut self,
        _messages: Vec<serde_json::Value>,
    ) -> ProxyFuture<'a, String> {
        let response = self.ai_response.clone();
        Box::pin(
            async move { response.ok_or_else(|| anyhow::anyhow!("no ai response configured")) },
        )
    }
}

impl crate::shell_capabilities::CronToolHost for TestShellProxy {
    fn cron_tool_call(
        &mut self,
        request: &dsh_types::cron::tool::CronToolRequest,
    ) -> Result<serde_json::Value> {
        self.cron_tool_calls.push(request.clone());
        if let Some(message) = &self.cron_tool_error {
            return Err(anyhow::anyhow!(message.clone()));
        }
        Ok(self
            .cron_tool_response
            .clone()
            .unwrap_or_else(|| serde_json::json!({})))
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

/// A task store that keeps everything in memory.
///
/// Enough to build an `AgentRuntime`, which is the only way to exercise the
/// persistent-task branches: those must never prompt, so a test that leaves
/// `agent_runtime` at `None` silently checks the interactive path instead.
#[derive(Default)]
pub(crate) struct MemoryTaskStore {
    tasks: Mutex<HashMap<String, dsh_types::agent::AgentTask>>,
    events: Mutex<Vec<(String, dsh_types::agent::TaskEvent)>>,
    artifacts: Mutex<HashMap<(String, String), serde_json::Value>>,
}

impl MemoryTaskStore {
    fn persist(
        &self,
        task: &dsh_types::agent::AgentTask,
        event: Option<(&str, &serde_json::Value)>,
    ) -> Result<crate::shell_capabilities::AgentTaskSave> {
        self.tasks.lock().insert(task.id.clone(), task.clone());
        let mut events = self.events.lock();
        if let Some((kind, data)) = event {
            let sequence = events.len() as u64 + 1;
            events.push((
                task.id.clone(),
                dsh_types::agent::TaskEvent {
                    sequence,
                    kind: kind.to_string(),
                    data: data.clone(),
                },
            ));
        }
        Ok(crate::shell_capabilities::AgentTaskSave {
            sequence: events.len() as u64,
            task: task.clone(),
        })
    }
}

impl crate::shell_capabilities::AgentTaskStore for MemoryTaskStore {
    fn save(
        &self,
        task: &dsh_types::agent::AgentTask,
        event: Option<(&str, &serde_json::Value)>,
    ) -> Result<crate::shell_capabilities::AgentTaskSave> {
        self.persist(task, event)
    }

    fn resume(
        &self,
        task: &dsh_types::agent::AgentTask,
        event: Option<(&str, &serde_json::Value)>,
    ) -> Result<crate::shell_capabilities::AgentTaskSave> {
        self.persist(task, event)
    }

    fn load(&self, id: &str) -> Result<dsh_types::agent::AgentTask> {
        self.tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no such task"))
    }

    fn list(&self) -> Result<Vec<dsh_types::agent::AgentTask>> {
        Ok(self.tasks.lock().values().cloned().collect())
    }

    fn events(&self, id: &str) -> Result<Vec<dsh_types::agent::TaskEvent>> {
        Ok(self
            .events
            .lock()
            .iter()
            .filter(|(task_id, _)| task_id == id)
            .map(|(_, event)| event.clone())
            .collect())
    }

    fn delete(&self, id: &str) -> Result<()> {
        self.tasks.lock().remove(id);
        Ok(())
    }

    fn save_artifact(&self, id: &str, name: &str, content: &serde_json::Value) -> Result<()> {
        self.artifacts
            .lock()
            .insert((id.to_string(), name.to_string()), content.clone());
        Ok(())
    }

    fn load_artifact(&self, id: &str, name: &str) -> Result<serde_json::Value> {
        self.artifacts
            .lock()
            .get(&(id.to_string(), name.to_string()))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no such artifact"))
    }
}

/// A task that is running, has a plan and has room in every budget, so a test
/// reaches the branch it is aiming at rather than an exhausted-budget refusal.
pub(crate) fn running_task(root: &std::path::Path) -> dsh_types::agent::AgentTask {
    dsh_types::agent::AgentTask {
        id: "test-task".to_string(),
        goal: "test".to_string(),
        root: root.to_path_buf(),
        status: dsh_types::agent::TaskStatus::Running,
        grant: dsh_types::agent::TaskGrant::default(),
        criteria: Vec::new(),
        plan: vec!["step".to_string()],
        progress: String::new(),
        tokens_used: 0,
        time_budget_ms: 600_000,
        elapsed_ms: 0,
        stop_reason: None,
        checkpoint: None,
        pending_operation: None,
        created_at: 0,
    }
}

/// An `AgentRuntime` backed by [`MemoryTaskStore`], ready to hand to a proxy.
///
/// The task is persisted first, the way `agent run` does it. `stopped()` treats
/// a task the store cannot load as stopped, so a runtime built without saving
/// reports "cancelled" for every command it is asked about - which a fast
/// command wins the race against and a slow one does not.
pub(crate) fn test_runtime(
    root: &std::path::Path,
) -> Arc<parking_lot::Mutex<crate::agent::AgentRuntime>> {
    let store = Arc::new(MemoryTaskStore::default());
    let task = running_task(root);
    {
        use crate::shell_capabilities::AgentTaskStore;
        store.save(&task, None).expect("in-memory save");
    }
    Arc::new(parking_lot::Mutex::new(crate::agent::AgentRuntime::new(
        task, store,
    )))
}
