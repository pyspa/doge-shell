//! Judging an MCP tool call: which tools only read, which ones run a command
//! (and so need the command itself judged), and the allowlist entries a session
//! approval is recorded under.
use super::*;

/// One MCP tool call, as the guard needs to see it.
///
/// A struct rather than a run of arguments because two of its fields are
/// `&str` that mean different things and read alike at a call site. Swapping
/// `function_name` and `tool_name` compiles and silently changes the verdict -
/// `mcp__ops__bash` never matches `"bash"`, so a shell tool stops being judged
/// as the command it runs. That regression has already happened once.
pub struct McpToolCall<'a> {
    /// The namespaced name the model called, e.g. `mcp__ops__bash`. What an
    /// allowlist entry and the question shown to the user are keyed on.
    pub function_name: &'a str,
    /// The tool's own name on its server, e.g. `bash`. What the danger
    /// classification looks at.
    pub tool_name: &'a str,
    pub args_json: &'a str,
    /// What the server's own listing said about side effects
    /// (`readOnlyHint: false` or `destructiveHint: true`).
    ///
    /// Believed only when it says `false`, i.e. only ever to close the gate:
    /// the server is the party this confirmation exists to protect against, so
    /// a claim it makes about itself must not be able to open it.
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

        if Self::is_read_only_mcp_tool(tool_name, declared_read_only) {
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

    /// Whether this tool may run at Normal without asking.
    ///
    /// The name is a guess - `search_and_replace` reads as a search - so a
    /// server that declares side effects is believed over it. The reverse is
    /// not true: `readOnlyHint: true` is *not* enough to skip the question,
    /// because the server is the party this confirmation exists to protect
    /// against, and a description a server writes about itself must never be
    /// able to open the gate. A declaration only ever closes it.
    ///
    /// The two marker lists are matched differently on purpose, and the
    /// asymmetry is the safety property:
    ///
    /// - **mutating markers match as substrings** - over-inclusive, so the
    ///   worst case is a question nobody needed.
    /// - **read markers match whole words** - under-inclusive, so the worst
    ///   case is again a question nobody needed.
    ///
    /// Matching read markers as substrings is what made this dangerous:
    /// `ls` occurs inside `emails`, `labels`, `channels` and `urls`, so
    /// `send_emails`, `add_labels` and `notify_channels` were all classified
    /// read-only and ran at Normal without asking.
    fn is_read_only_mcp_tool(tool_name: &str, declared_read_only: Option<bool>) -> bool {
        if declared_read_only == Some(false) {
            return false;
        }

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
            // Verbs a tool can be named for that the list above missed. Each
            // is a whole word in practice, so substring matching costs nothing
            // here beyond the occasional extra question.
            "replace",
            "send",
            "publish",
            "upload",
            "purge",
            "prune",
            "rotate",
            "provision",
            "revoke",
            "truncate",
            "rename",
            "sync",
            "drop",
            "clear",
        ];
        if mutating_markers.iter().any(|marker| name.contains(marker)) {
            return false;
        }

        let read_markers = [
            "list", "get", "read", "search", "find", "show", "status", "describe", "fetch",
            "query", "view", "ls", "stat",
        ];
        Self::words(tool_name)
            .iter()
            .any(|word| read_markers.contains(&word.as_str()))
    }

    /// The words in a tool name, lowercased, for markers that must not match
    /// inside one.
    ///
    /// Splits on the separators tool names use and on camelCase boundaries, so
    /// `listTools`, `list_tools`, `list-tools` and `getHTTPStatus` all yield
    /// their verb while `emails` yields only `emails`. Takes the original
    /// spelling: lowercasing first would erase the case boundary that makes
    /// `getFile` two words.
    pub(crate) fn words(name: &str) -> Vec<String> {
        let chars: Vec<char> = name.chars().collect();
        let mut words = Vec::new();
        let mut current = String::new();

        for (index, &c) in chars.iter().enumerate() {
            if !c.is_ascii_alphanumeric() {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                continue;
            }

            // A new word starts at lower→upper (`getFile`) and at the last
            // capital of an acronym run followed by a lowercase letter
            // (`getHTTPStatus` → `http`, `status`).
            let starts_word = c.is_ascii_uppercase()
                && !current.is_empty()
                && (chars[index - 1].is_ascii_lowercase()
                    || chars[index - 1].is_ascii_digit()
                    || chars
                        .get(index + 1)
                        .is_some_and(|next| next.is_ascii_lowercase()));
            if starts_word {
                words.push(std::mem::take(&mut current));
            }
            current.push(c.to_ascii_lowercase());
        }

        if !current.is_empty() {
            words.push(current);
        }
        words
    }
}
