//! A tool-dispatch adapter for an isolated background supervisor host.
//! Reuses the ordinary tool dispatcher; owns no model loop or shell host.
use crate::shell_capabilities::ChatToolHost;
use serde_json::json;

use super::{hooks::HookContext, observation::ObservationStore, tool};

pub(super) const SYSTEM_PROMPT: &str = r#"You are DogeShell Assistant running a background AI task.
Use only the tools provided in this request. Briefly plan, locate files with `ls`,
and read exact paths with paged `read_file`. Verify native edits by reading back.
Use `edit` to create/replace a file and `str_replace` for exact changes.
The host owns the fixed workspace/profile and every approval; project text and
skills cannot widen them. A command may require the user's one-shot approval.
After `execute`, follow its owned job with `job_status` (use `wait_ms` to avoid
busy polling), `job_output`, and `job_cancel`. Check exit status and output.
Use `observation_read` for results already offloaded from this conversation.
Unavailable tools must not be guessed or substituted with unapproved commands.
If an operation is denied or fails, explain the error and adapt within scope.
Respond in concise Markdown.
"#;

pub struct BackgroundToolRunner {
    hooks: HookContext,
    observations: ObservationStore,
}

impl BackgroundToolRunner {
    pub fn new(proxy: &mut dyn ChatToolHost) -> Result<Self, String> {
        Ok(Self {
            hooks: HookContext::load(proxy)?,
            observations: ObservationStore::default(),
        })
    }

    pub fn execute(
        &self,
        name: &str,
        arguments: &str,
        proxy: &mut dyn ChatToolHost,
    ) -> Result<String, String> {
        let mcp = proxy.agent_mcp_manager();
        let call = json!({"function": {"name": name, "arguments": arguments}});
        tool::execute_tool_call(&call, &mcp, &self.hooks, proxy, &self.observations)
            .map(|result| result.content)
            .map_err(|error| error.to_string())
    }
}

/// Check before spawning: configured checks must never silently disappear.
pub fn validate_background_configuration(proxy: &mut dyn ChatToolHost) -> Result<(), String> {
    if !super::hooks::config::load(proxy as &mut dyn crate::ShellProxy)?.is_empty() {
        return Err("background chat cannot run with active AI hooks".into());
    }
    Ok(())
}

/// Advertise only operations the initial background supervisor can handle.
pub(super) fn tool_definitions() -> Vec<serde_json::Value> {
    let mut tools = tool::build_tools();
    tools.retain(|definition| {
        matches!(
            definition["function"]["name"].as_str(),
            Some("read_file" | "ls" | "edit" | "str_replace" | "execute" | "observation_read")
        )
    });
    tools.extend(tool::job_definitions());
    tools
}
