//! Implicit MCP tool groups: one group per registered server.
//!
//! Today a group is exactly the tool set of one server, named for the server
//! label (for example group `github` holds `mcp__github__*`). While the mapping
//! stays 1:1, filtering by server label and reading the group's recorded
//! membership coincide; finer splits such as `github-read` / `github-write`
//! can later replace how membership is derived, at which point the label
//! comparisons in `group_tool_definitions` must follow `tool_groups()`.
//!
//! A group disable only hides schemas from the model. It is deliberately
//! separate from `disconnect`: the connection stays up and execution still
//! resolves through `bindings`, so this is exposure control, not a safety
//! boundary. The safety gate on actual tool calls is untouched.
use super::*;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpToolGroup {
    pub name: String,
    pub description: Option<String>,
    pub tools: Vec<String>,
    pub enabled: bool,
}

/// How much of the registered MCP surface is currently exposed to the model.
///
/// `schema_bytes` is the serialized length of the active definitions: a cheap
/// estimate, not a tokenizer count. Keep it behind this struct so a real token
/// measurement can replace it without touching callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct McpToolExposure {
    pub total_tools: usize,
    pub active_tools: usize,
    pub total_groups: usize,
    pub active_groups: usize,
    pub schema_bytes: usize,
}

impl McpManager {
    /// One implicit group per registered server, sorted by name.
    ///
    /// `enabled` is the group toggle alone: a group on a disconnected server
    /// still reports `enabled`, because exposure additionally requires the
    /// server to be connected. Membership lists every bound function name,
    /// including tools currently hidden by a disable.
    pub fn tool_groups(&self) -> Vec<McpToolGroup> {
        let group_disabled = self.group_disabled_read();
        let mut groups: Vec<McpToolGroup> = self
            .servers
            .iter()
            .map(|server| {
                let tools: Vec<String> = self
                    .bindings
                    .iter()
                    .filter(|(_, binding)| binding.server_label == server.label)
                    .map(|(name, _)| name.clone())
                    .collect();
                let description = server
                    .description
                    .clone()
                    .filter(|text| !text.trim().is_empty())
                    .or_else(|| Some(format!("Tools provided by MCP server '{}'", server.label)));
                McpToolGroup {
                    name: server.label.clone(),
                    description,
                    tools,
                    enabled: !group_disabled.contains(&server.label),
                }
            })
            .collect();
        groups.sort_by(|left, right| left.name.cmp(&right.name));
        groups
    }

    /// Whether this group's schemas are currently offered to the model.
    ///
    /// Unknown groups report `false`: nothing of theirs can be exposed.
    pub fn is_group_enabled(&self, group: &str) -> bool {
        self.servers.iter().any(|server| server.label == group)
            && !self.group_disabled_read().contains(group)
    }

    /// Offer a group's schemas to the model again.
    ///
    /// Returns `Ok(true)` when the group changed state, `Ok(false)` when it
    /// was already active. Fails on a disconnected server instead of
    /// reporting success while the tools stay hidden: exposure still requires
    /// the connection, so enabling here only lifts the group toggle.
    pub fn enable_group(&self, group: &str) -> Result<bool, String> {
        if !self.servers.iter().any(|server| server.label == group) {
            return Err(self.unknown_group_error(group));
        }
        if self.is_disabled(group) {
            return Err(format!(
                "MCP server '{group}' is disconnected; run `mcp connect {group}` to use its tools"
            ));
        }
        if self.group_disabled_write().remove(group) {
            let exposure = self.tool_exposure();
            debug!(
                group,
                active_tools = exposure.active_tools,
                active_groups = exposure.active_groups,
                "MCP group activated"
            );
            Ok(true)
        } else {
            debug!(group, "MCP group already active");
            Ok(false)
        }
    }

    /// Hide a group's schemas from the model, keeping the connection up.
    ///
    /// Returns `Ok(true)` when the group changed state, `Ok(false)` when it
    /// was already inactive.
    pub fn disable_group(&self, group: &str) -> Result<bool, String> {
        if !self.servers.iter().any(|server| server.label == group) {
            return Err(self.unknown_group_error(group));
        }
        if self.group_disabled_write().insert(group.to_string()) {
            let exposure = self.tool_exposure();
            debug!(
                group,
                active_tools = exposure.active_tools,
                active_groups = exposure.active_groups,
                "MCP group deactivated"
            );
            Ok(true)
        } else {
            debug!(group, "MCP group already inactive");
            Ok(false)
        }
    }

    fn unknown_group_error(&self, group: &str) -> String {
        let mut names: Vec<&str> = self
            .servers
            .iter()
            .map(|server| server.label.as_str())
            .collect();
        names.sort_unstable();
        debug!(group, "MCP group not found");
        if names.is_empty() {
            format!("Unknown MCP tool group: '{group}' (no MCP servers registered)")
        } else {
            format!(
                "Unknown MCP tool group: '{group}'\n\nAvailable groups:\n  {}",
                names.join("\n  ")
            )
        }
    }

    /// Every registered definition, including hidden and disconnected tools.
    ///
    /// The diagnostic view behind a future `mcp tools`: what exists, not what
    /// the model currently sees.
    pub fn all_tool_definitions(&self) -> Vec<Value> {
        self.definitions_matching(|_| true)
    }

    /// Only the definitions currently offered to the model.
    pub fn active_tool_definitions(&self) -> Vec<Value> {
        let disabled = self.disabled_read();
        let group_disabled = self.group_disabled_read();
        self.definitions_matching(|binding| {
            !disabled.contains(&binding.server_label)
                && !group_disabled.contains(&binding.server_label)
        })
    }

    /// The exposable definitions of one group: empty when the group is
    /// unknown, disabled, or disconnected.
    pub fn group_tool_definitions(&self, group: &str) -> Vec<Value> {
        let disabled = self.disabled_read();
        let group_disabled = self.group_disabled_read();
        self.definitions_matching(|binding| {
            binding.server_label == group
                && !disabled.contains(&binding.server_label)
                && !group_disabled.contains(&binding.server_label)
        })
    }

    /// Count of definitions currently offered to the model, without building
    /// the JSON schemas.
    ///
    /// Only counts bindings that resolve to a server tool, so this agrees
    /// with `active_tool_definitions().len()`.
    pub fn active_tool_count(&self) -> usize {
        let disabled = self.disabled_read();
        let group_disabled = self.group_disabled_read();
        self.bindings
            .values()
            .filter(|binding| {
                !disabled.contains(&binding.server_label)
                    && !group_disabled.contains(&binding.server_label)
                    && self.binding_resolves(binding)
            })
            .count()
    }

    /// Current exposure footprint for logs and `doctor`.
    pub fn tool_exposure(&self) -> McpToolExposure {
        let active = self.active_tool_definitions();
        let schema_bytes = serde_json::to_string(&active)
            .map(|text| text.len())
            .unwrap_or(0);
        let disabled = self.disabled_read();
        let group_disabled = self.group_disabled_read();
        let active_groups = self
            .servers
            .iter()
            .filter(|server| {
                !disabled.contains(&server.label) && !group_disabled.contains(&server.label)
            })
            .count();
        let exposure = McpToolExposure {
            total_tools: self
                .bindings
                .values()
                .filter(|binding| self.binding_resolves(binding))
                .count(),
            active_tools: active.len(),
            total_groups: self.servers.len(),
            active_groups,
            schema_bytes,
        };
        debug!(
            total = exposure.total_tools,
            active = exposure.active_tools,
            groups = exposure.active_groups,
            schema_bytes = exposure.schema_bytes,
            "MCP tool exposure"
        );
        exposure
    }

    /// Whether a binding still points at a registered server tool.
    ///
    /// `definitions_matching` answers the same question inline because it
    /// needs the references; the counters use this so a stale binding can
    /// never make a count disagree with the definitions built from it.
    fn binding_resolves(&self, binding: &ToolBinding) -> bool {
        self.servers
            .iter()
            .find(|server| server.label == binding.server_label)
            .is_some_and(|server| {
                server
                    .tools
                    .iter()
                    .any(|tool| tool.name.as_ref() == binding.tool_name)
            })
    }

    fn definitions_matching(&self, mut include: impl FnMut(&ToolBinding) -> bool) -> Vec<Value> {
        self.bindings
            .values()
            .filter_map(|binding| {
                if !include(binding) {
                    return None;
                }
                let server = self
                    .servers
                    .iter()
                    .find(|srv| srv.label == binding.server_label)?;
                let tool = server
                    .tools
                    .iter()
                    .find(|tool| tool.name.as_ref() == binding.tool_name)?;

                let schema = Value::Object((*tool.input_schema).clone());
                let description = match (&server.description, &tool.description) {
                    (Some(server_desc), Some(tool_desc)) if !server_desc.is_empty() => format!(
                        "MCP server `{}` — {}\nTool `{}`: {}",
                        server.label, server_desc, tool.name, tool_desc
                    ),
                    (Some(server_desc), _) if !server_desc.is_empty() => format!(
                        "MCP server `{}` — {}\nTool `{}`",
                        server.label, server_desc, tool.name
                    ),
                    (_, Some(tool_desc)) if !tool_desc.is_empty() => format!(
                        "MCP server `{}` tool `{}`: {}",
                        server.label, tool.name, tool_desc
                    ),
                    _ => format!("MCP server `{}` tool `{}`", server.label, tool.name),
                };

                let function_name = binding.function_name.clone();

                Some(json!({
                    "type": "function",
                    "function": {
                        "name": function_name,
                        "description": description,
                        "parameters": schema,
                    }
                }))
            })
            .collect()
    }
}
