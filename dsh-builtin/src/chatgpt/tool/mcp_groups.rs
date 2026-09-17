//! Meta tools for MCP tool groups: discover and activate them.
//!
//! Native doge-shell tools, not MCP-forwarded tools: they only read and flip
//! the group's exposure toggle inside `McpManager`. Flipping the toggle has
//! no external side effect - the connection stays as it was - so these run
//! without the approval gate that actual MCP tool calls go through. What the
//! toggle exposes still executes under the existing safety policy.
use crate::chatgpt::McpManager;
use serde_json::{Value, json};

pub(crate) const LIST_NAME: &str = "mcp_list_groups";
pub(crate) const LOAD_NAME: &str = "mcp_load_group";

pub(crate) fn definitions() -> Vec<Value> {
    vec![list_definition(), load_definition()]
}

pub(crate) fn list_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": LIST_NAME,
            "description": "List available MCP tool groups: each group's name, what it covers, how many tools it holds, and whether it is currently enabled. Call this first when the task needs an MCP capability none of your current tools covers, then activate exactly the groups you need with mcp_load_group.",
            "parameters": {
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn load_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": LOAD_NAME,
            "description": "Activate one MCP tool group so its tools become available to you. Use after mcp_list_groups when the task needs a capability your current tools do not cover. The group's tools are picked up on the next model request; a group that is already active needs no call.",
            "parameters": {
                "type": "object",
                "properties": {
                    "group": {
                        "type": "string",
                        "description": "Tool group name from mcp_list_groups."
                    }
                },
                "required": ["group"],
                "additionalProperties": false
            }
        }
    })
}

/// The group catalogue: name, coverage, size, and toggle state.
pub(crate) fn run_list(manager: &McpManager) -> String {
    let groups: Vec<Value> = manager
        .tool_groups()
        .into_iter()
        .map(|group| {
            json!({
                "name": group.name,
                "description": group.description,
                "tool_count": group.tools.len(),
                "enabled": group.enabled,
            })
        })
        .collect();
    json!({ "groups": groups }).to_string()
}

/// Flip one group's toggle. The newly exposed schemas reach the model on its
/// next request; the error text for unknown or disconnected groups already
/// tells it what to do instead.
pub(crate) fn run_load(manager: &McpManager, arguments: &str) -> Result<String, String> {
    let args: Value = serde_json::from_str(arguments).map_err(|err| err.to_string())?;
    let group = args
        .get("group")
        .and_then(Value::as_str)
        .ok_or("group required")?;
    match manager.enable_group(group) {
        Ok(true) => {
            let tool_count = manager.group_tool_definitions(group).len();
            Ok(json!({
                "status": "activated",
                "group": group,
                "tool_count": tool_count,
            })
            .to_string())
        }
        Ok(false) => Ok(json!({
            "status": "already_active",
            "group": group,
        })
        .to_string()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager_with_tools() -> McpManager {
        let mut manager = McpManager::default();
        manager.insert_test_tool("github", "list_issues");
        manager.insert_test_tool("github", "get_issue");
        manager.insert_test_tool("filesystem", "read_file");
        manager
    }

    #[test]
    fn list_reports_every_group_sorted_with_counts() {
        let rendered: Value =
            serde_json::from_str(&run_list(&manager_with_tools())).expect("valid JSON");

        assert_eq!(
            rendered,
            json!({
                "groups": [
                    {
                        "name": "filesystem",
                        "description": "filesystem server",
                        "tool_count": 1,
                        "enabled": true,
                    },
                    {
                        "name": "github",
                        "description": "github server",
                        "tool_count": 2,
                        "enabled": true,
                    },
                ]
            })
        );
    }

    #[test]
    fn list_marks_disabled_groups() {
        let manager = manager_with_tools();
        manager.disable_group("github").unwrap();

        let rendered: Value = serde_json::from_str(&run_list(&manager)).expect("valid JSON");
        let github = &rendered["groups"][1];
        assert_eq!(github["name"], "github");
        assert_eq!(github["enabled"], false);
        assert_eq!(rendered["groups"][0]["enabled"], true);
    }

    #[test]
    fn list_is_empty_without_servers() {
        assert_eq!(run_list(&McpManager::default()), r#"{"groups":[]}"#);
    }

    #[test]
    fn load_activates_a_disabled_group_and_reports_its_size() {
        let manager = manager_with_tools();
        manager.disable_group("github").unwrap();

        let rendered: Value =
            serde_json::from_str(&run_load(&manager, r#"{"group":"github"}"#).unwrap())
                .expect("valid JSON");
        assert_eq!(
            rendered,
            json!({
                "status": "activated",
                "group": "github",
                "tool_count": 2,
            })
        );
        assert!(manager.is_group_enabled("github"));
        assert_eq!(manager.active_tool_count(), 3);
    }

    #[test]
    fn load_reports_already_active_without_duplicating_tools() {
        let manager = manager_with_tools();

        let rendered: Value =
            serde_json::from_str(&run_load(&manager, r#"{"group":"github"}"#).unwrap())
                .expect("valid JSON");
        assert_eq!(
            rendered,
            json!({ "status": "already_active", "group": "github" })
        );
        assert_eq!(manager.active_tool_count(), 3);
    }

    #[test]
    fn load_rejects_unknown_groups_with_the_catalogue_hint() {
        let err = run_load(&manager_with_tools(), r#"{"group":"nope"}"#).unwrap_err();
        assert!(err.contains("Unknown MCP tool group: 'nope'"), "{err}");
        assert!(err.contains("github"), "{err}");
    }

    #[test]
    fn load_rejects_disconnected_servers_with_the_connect_hint() {
        let manager = manager_with_tools();
        manager.disconnect("github").unwrap();

        let err = run_load(&manager, r#"{"group":"github"}"#).unwrap_err();
        assert!(err.contains("disconnected"), "{err}");
        assert!(err.contains("mcp connect github"), "{err}");
    }

    #[test]
    fn load_requires_a_group() {
        let manager = manager_with_tools();
        assert!(run_load(&manager, "{}").is_err());
        assert!(run_load(&manager, r#"{"group":42}"#).is_err());
        assert!(run_load(&manager, "not json").is_err());
        // Nothing changed by the rejected calls.
        assert_eq!(manager.active_tool_count(), 3);
    }

    #[test]
    fn definitions_stay_small_fixed_shapes() {
        for definition in definitions() {
            assert_eq!(definition["type"], "function");
            let function = &definition["function"];
            assert_eq!(function["parameters"]["type"], "object");
            assert_eq!(function["parameters"]["additionalProperties"], false);
        }
        assert_eq!(list_definition()["function"]["name"], LIST_NAME);
        assert_eq!(
            load_definition()["function"]["parameters"]["required"],
            json!(["group"])
        );
        let total: usize = definitions()
            .iter()
            .map(|definition| definition.to_string().len())
            .sum();
        assert!(
            total < 2048,
            "meta tool schemas cost every request: {total}"
        );
    }
}
