//! Calling a tool on a connected MCP server: resolving the model's function
//! name back to its server and tool, running the call under the cancellation
//! flag, and the persistent-task operations that share the same binding table.
use super::*;

impl McpManager {
    pub fn execute_tool(&self, function_name: &str, arguments: &str) -> Result<String, String> {
        self.execute_tool_cancellable(function_name, arguments, &|| false, false)
            .map_err(|error| error.to_string())
    }
    pub fn execute_tool_cancellable(
        &self,
        function_name: &str,
        arguments: &str,
        cancel: &dyn Fn() -> bool,
        tasks: bool,
    ) -> std::result::Result<String, McpCallError> {
        let binding = match self.bindings.get(function_name) {
            Some(binding) => binding,
            None => {
                return Err(McpCallError::failure(format!(
                    "MCP tool binding `{function_name}` was not found"
                )));
            }
        };

        if self.disabled_read().contains(&binding.server_label) {
            return Err(McpCallError::failure(format!(
                "MCP server '{}' is disconnected; run `mcp connect {}` to use its tools",
                binding.server_label, binding.server_label
            )));
        }

        let args_value: Value = if arguments.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(arguments).map_err(|err| {
                McpCallError::failure(format!("failed to parse MCP tool arguments: {err}"))
            })?
        };

        let map = match args_value {
            Value::Null => None,
            Value::Object(map) => Some(map),
            other => {
                return Err(McpCallError::failure(format!(
                    "expected MCP tool arguments to be an object, got {other}"
                )));
            }
        };

        let transport = self
            .servers
            .iter()
            .find(|srv| srv.label == binding.server_label)
            .map(|srv| srv.transport.clone())
            .ok_or_else(|| McpCallError::failure("MCP server missing for tool invocation"))?;
        let tool_name = binding.tool_name.clone();

        let connection = {
            let mut pool = self.connections.lock();
            pool.entry(binding.server_label.clone())
                .or_insert_with(|| Arc::new(connection::Connection::new(transport)))
                .clone()
        };
        let mut params = json!({"name":tool_name,"arguments":map});
        if tasks {
            params["_meta"] = json!({"io.modelcontextprotocol/clientCapabilities":{"extensions":{"io.modelcontextprotocol/tasks":{}}}});
        }
        let value = connection
            .request(json!({"kind":"call","params":params,"tasks":tasks}), cancel)
            .map_err(|error| McpCallError::from_request(error, "failed to call MCP tool"))?;
        if value["resultType"] == "task" || value["resultType"] == "input_required" {
            return Ok(json!({"server":binding.server_label,"response":value}).to_string());
        }
        let result = serde_json::from_value(value)
            .map_err(|e| McpCallError::failure(format!("invalid MCP result: {e}")))?;
        render_tool_result(&result).map_err(McpCallError::failure)
    }

    pub fn task_operation(
        &self,
        server: &str,
        kind: &str,
        params: Value,
        cancel: &dyn Fn() -> bool,
    ) -> std::result::Result<Value, McpCallError> {
        if self.is_disabled(server) {
            return Err(McpCallError::failure("MCP server is disconnected"));
        }
        let transport = self
            .servers
            .iter()
            .find(|s| s.label == server)
            .ok_or_else(|| McpCallError::failure("unknown MCP server"))?
            .transport
            .clone();
        let connection = self
            .connections
            .lock()
            .entry(server.to_string())
            .or_insert_with(|| Arc::new(connection::Connection::new(transport)))
            .clone();
        connection
            .request(json!({"kind":kind,"params":params}), cancel)
            .map_err(|error| McpCallError::from_request(error, "MCP operation failed"))
    }

    pub(crate) fn has_tool_binding(&self, function_name: &str) -> bool {
        self.bindings.contains_key(function_name)
    }

    /// The tool's own name behind the `mcp__<label>__<tool>` function name.
    ///
    /// Kept for diagnostics and existence checks only. Authorization must use
    /// [`McpManager::tool_facts_for`]: the trust of the owning server is part
    /// of the verdict, and a bare name cannot carry it.
    pub fn tool_name_for(&self, function_name: &str) -> Option<String> {
        self.bindings
            .get(function_name)
            .map(|binding| binding.tool_name.clone())
    }

    /// The binding plus the owning server's trust, from one lookup.
    ///
    /// All authorization paths judge one call from this single snapshot.
    /// Reading the name and the declaration separately lets a concurrent
    /// `mcp connect` or tool-list refresh replace the binding in between;
    /// reading trust separately would additionally let a trust change land
    /// on one entry point and not the other.
    ///
    /// Returns `None` when the binding - or the server behind it - cannot be
    /// resolved. Callers must treat that as untrusted (fail closed), never
    /// as a reason to skip the question.
    pub fn tool_facts_for(&self, function_name: &str) -> Option<McpToolFacts> {
        let binding = self.bindings.get(function_name)?;
        let server = self
            .servers
            .iter()
            .find(|server| server.label == binding.server_label)?;
        Some(McpToolFacts {
            server_label: server.label.clone(),
            server_trust: server.trust,
            tool_name: binding.tool_name.clone(),
            declared_read_only: binding.declared_read_only,
        })
    }

    #[cfg(test)]
    pub(crate) fn insert_test_tool_binding(&mut self, function_name: &str) {
        let server_label = "test".to_string();
        let tool_name = "tool".to_string();

        if !self
            .servers
            .iter()
            .any(|server| server.label == server_label)
        {
            self.servers.push(McpServer {
                label: server_label.clone(),
                description: None,
                trust: dsh_types::mcp::McpServerTrust::Untrusted,
                transport: McpTransport::Sse {
                    url: "http://localhost.invalid/sse".to_string(),
                },
                tools: Vec::new(),
            });
        }

        self.bindings.insert(
            function_name.to_string(),
            ToolBinding {
                server_label,
                tool_name,
                function_name: function_name.to_string(),
                declared_read_only: None,
            },
        );
    }

    /// Register a real tool on a stub server for tests: the server carries
    /// the tool in its listing and the binding table points at it, so both
    /// group membership and resolvable definitions agree.
    #[cfg(test)]
    pub(crate) fn insert_test_tool(&mut self, label: &str, tool: &str) {
        self.insert_test_tool_full(
            label,
            tool,
            &format!("{tool} description"),
            serde_json::json!({"type": "object"}),
        );
    }

    /// Like [`McpManager::insert_test_tool`], with an explicit description
    /// and input schema, for tests that rank over parameter metadata.
    #[cfg(test)]
    pub(crate) fn insert_test_tool_full(
        &mut self,
        label: &str,
        tool: &str,
        description: &str,
        schema: serde_json::Value,
    ) {
        if !self.servers.iter().any(|server| server.label == label) {
            self.servers.push(McpServer {
                label: label.to_string(),
                description: Some(format!("{label} server")),
                trust: dsh_types::mcp::McpServerTrust::Untrusted,
                transport: McpTransport::Sse {
                    url: "http://localhost.invalid/sse".to_string(),
                },
                tools: Vec::new(),
            });
        }
        let input_schema = schema.as_object().cloned().unwrap_or_default();
        let rmcp_tool = Tool::new(
            tool.to_string(),
            description.to_string(),
            std::sync::Arc::new(input_schema),
        );
        let server = self
            .servers
            .iter_mut()
            .find(|server| server.label == label)
            .expect("server inserted above");
        if !server.tools.iter().any(|known| known.name.as_ref() == tool) {
            server.tools.push(rmcp_tool.clone());
        }
        let (function_name, binding) = super::bind_tool(label, &rmcp_tool);
        self.bindings.insert(function_name, binding);
    }
}
