//! Judging an MCP tool call: untrusted servers always ask, trusted servers
//! may use an explicit read-only annotation, and command-execution tools go
//! through the normal command classifier.
use super::*;
use dsh_types::mcp::McpServerTrust;

/// One MCP tool call, as the guard needs to see it.
///
/// A struct rather than a run of arguments because two of its fields are
/// `&str` that mean different things and read alike at a call site. Swapping
/// `function_name` and `tool_name` compiles and silently changes the verdict -
/// `mcp__ops__bash` never matches `"bash"`, so a shell tool stops being judged
/// as the command it runs. That regression has already happened once.
///
/// `server_trust` is the explicit operator opt-in for this server, resolved by
/// `McpManager::tool_facts_for` from the owning `McpServer` in the same lookup
/// as the name and the annotation. Tool names and `readOnlyHint` are both
/// server-controlled, so neither may open the Normal gate on its own: only a
/// `Trusted` server's `readOnlyHint: true` counts as a positive signal, while
/// a `false`/destructive declaration always tightens the gate.
pub struct McpToolCall<'a> {
    /// The namespaced name the model called, e.g. `mcp__ops__bash`. What an
    /// allowlist entry and the question shown to the user are keyed on.
    pub function_name: &'a str,
    /// The label of the server that owns the tool, e.g. `ops`. Shown in the
    /// confirmation so the operator sees whose trust they are spending.
    pub server_label: &'a str,
    /// The operator's explicit trust for that server. `Untrusted` is the
    /// default; unknown bindings must fall back here (fail closed).
    pub server_trust: McpServerTrust,
    /// The tool's own name on its server, e.g. `bash`. Used to recognise
    /// command-execution tools - never as a read-only signal.
    pub tool_name: &'a str,
    pub args_json: &'a str,
    /// What the server's own listing said about side effects
    /// (`readOnlyHint: false` or `destructiveHint: true`).
    ///
    /// Believed only to close the gate on untrusted servers, and to open the
    /// read-only path on explicitly trusted ones. A missing annotation never
    /// opens the gate.
    pub declared_read_only: Option<bool>,
}

impl SafetyGuard {
    pub fn check_mcp_tool(
        &self,
        call: McpToolCall<'_>,
        level: &SafetyLevel,
        allowlist: &[String],
    ) -> SafetyResult {
        let McpToolCall {
            function_name,
            server_label,
            server_trust,
            tool_name,
            args_json,
            declared_read_only,
        } = call;
        if matches!(level, SafetyLevel::Loose) {
            return SafetyResult::Allowed;
        }

        if Self::is_allowlisted_mcp_call(function_name, args_json, allowlist) {
            return SafetyResult::Allowed;
        }

        // The exact command string behind a command-execution tool, when the
        // tool is one and carries a non-empty one. Extracted once: the
        // allowlist probe below and the classifier both read it.
        let mcp_command: Option<String> = if Self::is_mcp_command_execution_tool(tool_name) {
            Self::extract_mcp_command(args_json).filter(|cmd| !cmd.trim().is_empty())
        } else {
            None
        };

        // An exact operator approval of that command string opens the gate at
        // any level and trust. This is the operator's own verbatim string, not
        // server-controlled classification, so the trust boundary does not
        // apply to it - the same as before trust existed (in particular, this
        // keeps bare-command approvals working in Strict mode as they did).
        if let Some(cmd_str) = &mcp_command
            && allowlist.contains(cmd_str)
        {
            return SafetyResult::Allowed;
        }

        if matches!(level, SafetyLevel::Strict) {
            return SafetyResult::Confirm(format!(
                "MCP tool '{function_name}' execution requested in Strict mode."
            ));
        }

        // Normal from here on. Untrusted servers never auto-run: the tool
        // name, the annotation, and any embedded command are all
        // server-controlled, so none of them may open the gate.
        let trusted = matches!(server_trust, McpServerTrust::Trusted);
        if !trusted {
            return SafetyResult::Confirm(format!(
                "MCP tool '{function_name}' from untrusted server '{server_label}' requires confirmation."
            ));
        }

        // Trusted servers only from here on.
        //
        // A server that declares side effects is always believed in the
        // strict direction, even when the command it carries looks benign.
        if declared_read_only == Some(false) {
            return SafetyResult::Confirm(format!(
                "MCP tool '{function_name}' from server '{server_label}' declares side effects."
            ));
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
            let Some(cmd_str) = mcp_command else {
                return SafetyResult::Confirm(format!(
                    "MCP tool '{function_name}' from server '{server_label}' wants to execute a command, but arguments could not be validated safely."
                ));
            };
            return match self.classify_command_line(&cmd_str) {
                Some(reason) => {
                    // The classifier reasons end in "Proceed?", and the
                    // confirmation UI appends its own prompt - keep only one.
                    let reason = reason.strip_suffix("Proceed?").unwrap_or(&reason);
                    let reason = reason.trim_end();
                    SafetyResult::Confirm(format!(
                        "MCP tool '{function_name}' from server '{server_label}' wants to execute command '{cmd_str}': {reason}"
                    ))
                }
                None => SafetyResult::Allowed,
            };
        }

        if declared_read_only == Some(true) {
            SafetyResult::Allowed
        } else {
            SafetyResult::Confirm(format!(
                "MCP tool '{function_name}' from server '{server_label}' has no trusted read-only declaration."
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
}
