//! Judging an MCP tool call: which tools only read, which ones run a command
//! (and so need the command itself judged), and the allowlist entries a session
//! approval is recorded under.
use super::*;

impl SafetyGuard {
    pub fn check_mcp_tool(
        &self,
        function_name: &str,
        tool_name: &str,
        args_json: &str,
        level: &SafetyLevel,
        allowlist: &[String],
    ) -> SafetyResult {
        if matches!(level, SafetyLevel::Loose) {
            return SafetyResult::Allowed;
        }

        if Self::is_allowlisted_mcp_call(function_name, args_json, allowlist) {
            return SafetyResult::Allowed;
        }

        // If it is a command execution tool, judge the raw command line
        // itself - the same way a typed command is judged, not through
        // `check_command`. `check_command` exists for callers that were
        // already handed separate tokens with no shell text behind them;
        // this `cmd_str` *is* shell text (an MCP tool's `command` argument
        // works exactly like the `execute` tool's), so tokenizing it here
        // and reassembling `(cmd, args)` only to have `check_command`
        // rejoin them with spaces and tokenize *again* would risk turning a
        // quote or a `;`/`|` that was legitimately part of one argument's
        // value into a fabricated command boundary.
        if Self::is_mcp_command_execution_tool(tool_name) {
            let Some(cmd_str) = Self::extract_mcp_command(args_json) else {
                return SafetyResult::Confirm(format!(
                    "MCP tool '{}' requested command execution, but arguments could not be validated safely. Proceed?",
                    function_name
                ));
            };
            if cmd_str.trim().is_empty() {
                return SafetyResult::Confirm(format!(
                    "MCP tool '{}' requested command execution, but arguments could not be validated safely. Proceed?",
                    function_name
                ));
            }
            if allowlist.contains(&cmd_str) {
                return SafetyResult::Allowed;
            }
            if matches!(level, SafetyLevel::Strict) {
                return SafetyResult::Confirm(format!(
                    "Command '{}' will be executed. Proceed?",
                    cmd_str
                ));
            }
            return match self.classify_command_line(&cmd_str) {
                Some(reason) => SafetyResult::Confirm(reason),
                None => SafetyResult::Allowed,
            };
        }

        if matches!(level, SafetyLevel::Strict) {
            return SafetyResult::Confirm(format!(
                "MCP tool '{}' execution requested in Strict mode. Proceed?",
                function_name
            ));
        }

        if Self::is_read_only_mcp_tool(tool_name) {
            SafetyResult::Allowed
        } else {
            SafetyResult::Confirm(format!(
                "MCP tool '{}' may have side effects. Proceed?",
                function_name
            ))
        }
    }

    pub fn mcp_allowlist_entry(tool_name: &str, args_json: &str) -> String {
        let normalized_args = serde_json::from_str::<serde_json::Value>(args_json)
            .ok()
            .and_then(|v| serde_json::to_string(&v).ok())
            .unwrap_or_else(|| args_json.trim().to_string());
        format!("mcp:{}:{}", tool_name, normalized_args)
    }

    fn mcp_allowlist_prefix(tool_name: &str) -> String {
        format!("mcp:{}", tool_name)
    }

    fn is_allowlisted_mcp_call(tool_name: &str, args_json: &str, allowlist: &[String]) -> bool {
        let exact = Self::mcp_allowlist_entry(tool_name, args_json);
        if allowlist.contains(&exact) {
            return true;
        }

        let wildcard = Self::mcp_allowlist_prefix(tool_name);
        allowlist.contains(&wildcard)
    }

    fn is_mcp_command_execution_tool(tool_name: &str) -> bool {
        matches!(
            tool_name,
            "bash" | "run_command" | "execute_command" | "execute" | "terminal"
        )
    }

    fn extract_mcp_command(args_json: &str) -> Option<String> {
        let json_val = serde_json::from_str::<serde_json::Value>(args_json).ok()?;
        json_val
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn is_read_only_mcp_tool(tool_name: &str) -> bool {
        let name = tool_name.to_ascii_lowercase();
        let mutating_markers = [
            "write",
            "edit",
            "update",
            "delete",
            "remove",
            "create",
            "execute",
            "run",
            "apply",
            "install",
            "set",
            "post",
            "put",
            "patch",
            "push",
            "kill",
            "start",
            "stop",
            "restart",
            "connect",
            "disconnect",
            "submit",
        ];
        if mutating_markers.iter().any(|marker| name.contains(marker)) {
            return false;
        }

        let read_markers = [
            "list", "get", "read", "search", "find", "show", "status", "describe", "fetch",
            "query", "view", "ls", "stat",
        ];
        read_markers.iter().any(|marker| name.contains(marker))
    }
}
