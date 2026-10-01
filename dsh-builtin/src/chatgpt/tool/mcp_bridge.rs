//! MCP stable tool bridge: `tool_search` / `tool_describe` / `tool_call`.
//!
//! For large MCP catalogs the model must not receive individual `mcp__*`
//! schemas in the request `tools` array. This module is the definition and
//! parsing half of that trade: the three fixed bridge definitions, the
//! on-demand schema loader (`tool_describe`), and the wire-call to
//! logical-call normalization behind `tool_call`.
//!
//! `tool_call` is a transport adapter only. It never executes anything
//! itself: [`resolve_wire_call`] unwraps the wrapper into the logical MCP
//! call *before* `AgentRuntime::before_tool`, hooks, authorization,
//! `SafetyGuard`, approvals, and `McpManager` execution, so every one of
//! those sees the underlying `mcp__server__tool` - never `tool_call`.
//! Search ranking itself stays in [`super::tool_search`]; nothing here
//! reimplements it.
use super::super::McpManager;
use serde_json::{Value, json};

/// Loads full schemas for exact MCP function names returned by `tool_search`.
pub(crate) const DESCRIBE_NAME: &str = "tool_describe";
/// Invokes one discovered MCP tool without its schema in the request.
pub(crate) const CALL_NAME: &str = "tool_call";

/// Ceiling on names per `tool_describe` call: one call must not reintroduce
/// the dump-every-schema behaviour the bridge exists to avoid.
pub(crate) const MAX_DESCRIBE_NAMES: usize = 10;
/// Whole-schema budget per `tool_describe` call, in compact-JSON serialized
/// UTF-8 bytes (not tokens, so no tokenizer is needed). Admission is
/// definition-by-definition: a schema is either included whole or skipped
/// whole, never truncated mid-JSON.
pub(crate) const MAX_DESCRIBE_SCHEMA_BYTES: usize = 96 * 1024;

/// The fixed bridge surface: exactly these three definitions, byte-stable
/// for the whole turn. No `mcp__*` schema, no `mcp_list_groups`, no
/// `mcp_load_group`.
pub(crate) fn bridge_definitions() -> Vec<Value> {
    vec![
        super::tool_search::bridge_definition(),
        describe_definition(),
        call_definition(),
    ]
}

/// Whether `name` is bridge machinery rather than a real tool.
pub(crate) fn is_bridge_tool(name: &str) -> bool {
    matches!(name, "tool_search" | DESCRIBE_NAME | CALL_NAME)
}

/// Schema definition for `tool_describe`: full schemas on demand, never
/// authorization or execution.
pub(crate) fn describe_definition() -> Value {
    crate::agent::definition(
        DESCRIBE_NAME,
        "Load full argument schemas for exact MCP function names returned by tool_search. Use it when you need a selected tool's parameters, then invoke the tool through tool_call. Describing a tool does not authorize or execute it.",
        json!({"names":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":10,"description":"Exact MCP function names from tool_search results."}}),
        &["names"],
    )
}

/// Schema definition for `tool_call`: exactly one underlying MCP invocation
/// per call. No batching: batch execution belongs with a future programmatic
/// pipeline, where partial success, approval ordering, and task handles can
/// be answered together.
pub(crate) fn call_definition() -> Value {
    crate::agent::definition(
        CALL_NAME,
        "Invoke one MCP tool discovered through tool_search (describe its schema first with tool_describe when needed). The call still passes the shell's safety policy and may be refused.",
        json!({"name":{"type":"string","description":"Exact MCP function name returned by tool_search"},"arguments":{"type":"object","description":"Arguments for the selected MCP tool"}}),
        &["name", "arguments"],
    )
}

/// Run the `tool_describe` tool: resolve exact names to complete schemas.
///
/// Refresh is the caller's job (`dispatch_tool` runs
/// `refresh_tools_if_expired` first, the same as `tool_search`), so a tool
/// removed after a previous search resolves to `missing` instead of serving
/// a stale copy. `McpManager` stays the single authority: nothing here
/// caches schemas. Unknown, disconnected, or stale names land in `missing`
/// without failing the names that did resolve.
pub(crate) fn run_describe(manager: &McpManager, arguments: &str) -> Result<String, String> {
    let args: Value = serde_json::from_str(arguments).map_err(|err| err.to_string())?;
    let names = args
        .get("names")
        .and_then(Value::as_array)
        .ok_or("names must be an array of exact MCP function names")?;
    if names.is_empty() || names.len() > MAX_DESCRIBE_NAMES {
        return Err(format!(
            "names must contain 1 to {MAX_DESCRIBE_NAMES} exact MCP function names"
        ));
    }
    let mut requested = Vec::with_capacity(names.len());
    for name in names {
        let name = name
            .as_str()
            .ok_or("each entry of names must be an exact MCP function name string")?;
        if name.trim().is_empty() {
            return Err("each entry of names must be a nonempty MCP function name".into());
        }
        requested.push(name.to_string());
    }

    let mut tools = Vec::new();
    let mut missing = Vec::new();
    let mut skipped_budget: Vec<String> = Vec::new();
    let mut oversized = Vec::new();
    let mut used_bytes = 0usize;
    for name in &requested {
        let mut resolved = manager.tool_definitions_for(std::slice::from_ref(name));
        let Some(definition) = resolved.pop() else {
            missing.push(name.clone());
            continue;
        };
        let bytes = serde_json::to_vec(&definition)
            .map(|body| body.len())
            .unwrap_or(0);
        if used_bytes + bytes > MAX_DESCRIBE_SCHEMA_BYTES {
            // Whole schemas only: never partially include a definition, and
            // never silently shrink its parameter list. A schema that does
            // not fit even on its own is reported with its size.
            skipped_budget.push(name.clone());
            if bytes > MAX_DESCRIBE_SCHEMA_BYTES {
                oversized.push(json!({"name": name, "schema_bytes": bytes}));
            }
            continue;
        }
        used_bytes += bytes;
        tools.push(definition);
    }

    let count = tools.len();
    let mut result = json!({
        "count": count,
        "tools": tools,
        "missing": missing,
        "skipped_budget": skipped_budget,
    });
    if !oversized.is_empty() {
        result["oversized"] = Value::Array(oversized);
    }
    Ok(result.to_string())
}

/// A provider wire call normalized for policy and execution.
///
/// For an ordinary tool the two are identical. For a `tool_call` wrapper the
/// wire call is what the provider asked for (and what the conversation
/// history must keep, so the tool result still pairs with its outer id),
/// while the logical call is the unwrapped `mcp__server__tool` invocation
/// every policy boundary below must see instead.
#[derive(Debug)]
pub(crate) struct ResolvedToolCall {
    pub wire_call: Value,
    pub logical_call: Value,
}

/// Normalize one wire call before `AgentRuntime::before_tool`.
///
/// Non-bridge calls pass through with `logical_call == wire_call`. A
/// `tool_call` wrapper becomes the logical MCP call carrying the same outer
/// id and the serialized inner arguments object. Anything malformed -
/// missing name, non-object arguments, a builtin or bridge name smuggled
/// through the bridge, an unknown or disconnected MCP target - is an
/// `Err` the caller reports as an ordinary, actionable tool failure: never
/// a panic, never an internal retry.
///
/// Only names resolving to a current MCP binding are valid: the bridge is
/// an MCP adapter, not a universal tool dispatcher.
pub(crate) fn resolve_wire_call(
    wire_call: &Value,
    manager: &McpManager,
) -> Result<ResolvedToolCall, String> {
    let function = wire_call.get("function");
    let wire_name = function
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if wire_name != CALL_NAME {
        return Ok(ResolvedToolCall {
            wire_call: wire_call.clone(),
            logical_call: wire_call.clone(),
        });
    }

    let wire_id = wire_call.get("id").cloned().unwrap_or(Value::Null);
    let args_text = function
        .and_then(|function| function.get("arguments"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args: Value = serde_json::from_str(args_text)
        .map_err(|err| format!("invalid `tool_call` arguments: {err}"))?;
    let target = args.get("name").and_then(Value::as_str).unwrap_or_default();
    if target.trim().is_empty() {
        return Err(
            "`tool_call` requires the exact MCP function `name` returned by `tool_search`."
                .to_string(),
        );
    }
    if is_bridge_tool(target) {
        return Err(format!(
            "`{target}` is bridge machinery, not an MCP tool. Call it directly."
        ));
    }
    let Some(target_args) = args.get("arguments") else {
        return Err(format!(
            "`tool_call` requires `arguments` to be an object for MCP tool `{target}`."
        ));
    };
    if !target_args.is_object() {
        return Err(format!(
            "`tool_call` requires `arguments` to be an object for MCP tool `{target}`."
        ));
    }
    match manager.tool_facts_for(target) {
        None if !target.starts_with("mcp__") => {
            return Err(format!(
                "`{target}` is not a deferred MCP tool. Call it directly."
            ));
        }
        None => {
            return Err(format!(
                "MCP tool `{target}` is unavailable. Run tool_search again."
            ));
        }
        Some(facts) if manager.is_disabled(&facts.server_label) => {
            return Err(format!(
                "MCP tool `{target}` is unavailable while its server is disconnected. Run tool_search again."
            ));
        }
        Some(_) => {}
    }

    Ok(ResolvedToolCall {
        wire_call: wire_call.clone(),
        logical_call: json!({
            "id": wire_id,
            "type": "function",
            "function": {
                "name": target,
                "arguments": target_args.to_string(),
            }
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge_wire(id: &str, name: &str, arguments: Value) -> Value {
        json!({
            "id": id,
            "type": "function",
            "function": {
                "name": "tool_call",
                "arguments": json!({"name": name, "arguments": arguments}).to_string(),
            }
        })
    }

    #[test]
    fn bridge_definitions_are_exactly_the_stable_trio() {
        let definitions = bridge_definitions();
        let names: Vec<&str> = definitions
            .iter()
            .filter_map(|definition| definition["function"]["name"].as_str())
            .collect();
        assert_eq!(names, vec!["tool_search", "tool_describe", "tool_call"]);
        for definition in &definitions {
            assert_eq!(definition["type"], "function");
            assert_eq!(definition["function"]["parameters"]["type"], "object");
            assert_eq!(
                definition["function"]["parameters"]["additionalProperties"],
                false
            );
        }
    }

    #[test]
    fn bridge_search_definition_promises_no_schema_exposure() {
        let definition = super::super::tool_search::bridge_definition();
        let description = definition["function"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(description.contains("tool_describe"), "{description}");
        assert!(description.contains("tool_call"), "{description}");
        assert!(!description.contains("next model request"), "{description}");
        assert!(!description.contains("mcp_load_group"), "{description}");
    }

    #[test]
    fn describe_returns_the_complete_schema_for_a_known_tool() {
        let mut manager = McpManager::default();
        manager.insert_test_tool_full(
            "github",
            "list_issues",
            "list issues",
            json!({"type": "object", "properties": {"state": {"type": "string"}}}),
        );

        let rendered: Value = serde_json::from_str(
            &run_describe(&manager, r#"{"names":["mcp__github__list_issues"]}"#).unwrap(),
        )
        .unwrap();
        assert_eq!(rendered["count"], 1);
        assert_eq!(
            rendered["tools"][0]["function"]["name"],
            "mcp__github__list_issues"
        );
        assert_eq!(
            rendered["tools"][0]["function"]["parameters"]["properties"]["state"]["type"],
            "string"
        );
        assert_eq!(rendered["missing"], json!([]));
        assert_eq!(rendered["skipped_budget"], json!([]));
    }

    #[test]
    fn describe_reports_unknown_names_without_failing_the_known_ones() {
        let mut manager = McpManager::default();
        manager.insert_test_tool("github", "list_issues");

        let rendered: Value = serde_json::from_str(
            &run_describe(
                &manager,
                r#"{"names":["mcp__github__list_issues","mcp__nope__missing"]}"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(rendered["count"], 1);
        assert_eq!(rendered["missing"], json!(["mcp__nope__missing"]));
    }

    #[test]
    fn describe_reaches_hidden_group_tools_and_skips_disconnected_ones() {
        let mut manager = McpManager::default();
        manager.insert_test_tool("github", "list_issues");
        manager.insert_test_tool("ops", "deploy");
        manager.disable_group("github").unwrap();
        manager.disconnect("ops").unwrap();

        let rendered: Value = serde_json::from_str(
            &run_describe(
                &manager,
                r#"{"names":["mcp__github__list_issues","mcp__ops__deploy"]}"#,
            )
            .unwrap(),
        )
        .unwrap();
        // Group hiding is progressive exposure, not authorization: still
        // describable. A disconnected server resolves to nothing.
        assert_eq!(rendered["count"], 1);
        assert_eq!(
            rendered["tools"][0]["function"]["name"],
            "mcp__github__list_issues"
        );
        assert_eq!(rendered["missing"], json!(["mcp__ops__deploy"]));
    }

    #[test]
    fn describe_skips_whole_schemas_over_budget_and_names_oversized_ones() {
        let mut manager = McpManager::default();
        manager.insert_test_tool_full(
            "big",
            "huge_tool",
            &"x".repeat(100_000),
            json!({"type": "object"}),
        );
        manager.insert_test_tool("big", "note_tool");

        let rendered: Value = serde_json::from_str(
            &run_describe(
                &manager,
                r#"{"names":["mcp__big__note_tool","mcp__big__huge_tool"]}"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(rendered["count"], 1);
        // The admitted schema is whole: its description survived intact.
        assert_eq!(
            rendered["tools"][0]["function"]["name"],
            "mcp__big__note_tool"
        );
        assert_eq!(rendered["skipped_budget"], json!(["mcp__big__huge_tool"]));
        // A schema that fits nowhere on its own is reported with its size,
        // never silently shrunk.
        assert_eq!(rendered["oversized"][0]["name"], "mcp__big__huge_tool");
        assert!(
            rendered["oversized"][0]["schema_bytes"].as_u64().unwrap()
                > MAX_DESCRIBE_SCHEMA_BYTES as u64
        );
        // Still valid JSON with complete definitions only.
        for tool in rendered["tools"].as_array().unwrap() {
            assert!(tool["function"]["name"].is_string());
            assert!(tool["function"]["parameters"].is_object());
        }
    }

    #[test]
    fn describe_rejects_bad_shapes() {
        let manager = McpManager::default();
        assert!(run_describe(&manager, r#"{"names":[]}"#).is_err());
        assert!(run_describe(&manager, r#"{}"#).is_err());
        assert!(run_describe(&manager, r#"{"names":[""]}"#).is_err());
        assert!(run_describe(&manager, r#"{"names":[42]}"#).is_err());
        let too_many = (0..MAX_DESCRIBE_NAMES + 1)
            .map(|index| format!("\"mcp__bulk__tool_{index}\""))
            .collect::<Vec<_>>()
            .join(",");
        assert!(run_describe(&manager, &format!("{{\"names\":[{too_many}]}}")).is_err());
    }

    #[test]
    fn resolve_unwraps_a_bridge_call_into_its_logical_call() {
        let mut manager = McpManager::default();
        manager.insert_test_tool("github", "list_issues");
        let wire = bridge_wire(
            "call-9",
            "mcp__github__list_issues",
            json!({"state": "open"}),
        );

        let resolved = resolve_wire_call(&wire, &manager).unwrap();
        assert_eq!(resolved.wire_call, wire);
        assert_eq!(resolved.logical_call["id"], "call-9");
        assert_eq!(
            resolved.logical_call["function"]["name"],
            "mcp__github__list_issues"
        );
        assert_eq!(
            resolved.logical_call["function"]["arguments"],
            r#"{"state":"open"}"#
        );
    }

    #[test]
    fn resolve_passes_ordinary_calls_through_unchanged() {
        let manager = McpManager::default();
        let wire = json!({
            "id": "call-1",
            "type": "function",
            "function": {"name": "read_file", "arguments": "{}"},
        });

        let resolved = resolve_wire_call(&wire, &manager).unwrap();
        assert_eq!(resolved.logical_call, wire);
    }

    #[test]
    fn resolve_rejects_everything_that_is_not_a_current_mcp_binding() {
        let mut manager = McpManager::default();
        manager.insert_test_tool("github", "list_issues");
        manager.insert_test_tool("ops", "deploy");
        manager.disconnect("ops").unwrap();

        // Missing or empty target name.
        assert!(
            resolve_wire_call(
                &json!({"id":"c","function":{"name":"tool_call","arguments":"{}"}}),
                &manager
            )
            .is_err()
        );
        // Non-object arguments.
        assert!(
            resolve_wire_call(
                &bridge_wire("c", "mcp__github__list_issues", json!("state")),
                &manager
            )
            .is_err()
        );
        // Builtins and bridge machinery are not valid targets.
        for forbidden in [
            "read_file",
            "execute",
            "tool_call",
            "tool_describe",
            "tool_search",
            "mcp_load_group",
        ] {
            let err =
                resolve_wire_call(&bridge_wire("c", forbidden, json!({})), &manager).unwrap_err();
            assert!(err.contains("directly"), "{forbidden}: {err}");
        }
        // Unknown and disconnected MCP names.
        for unavailable in ["mcp__nope__missing", "mcp__ops__deploy"] {
            let err =
                resolve_wire_call(&bridge_wire("c", unavailable, json!({})), &manager).unwrap_err();
            assert!(err.contains("tool_search"), "{unavailable}: {err}");
        }
    }
}
