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
    /// The safety guard classifies a tool by its name, and the name the model
    /// calls is namespaced: matching `"bash"` against `mcp__ops__bash` never
    /// held, so a server's shell tool was judged as a generic side effect
    /// rather than as the command it was about to run.
    pub fn tool_name_for(&self, function_name: &str) -> Option<String> {
        self.bindings
            .get(function_name)
            .map(|binding| binding.tool_name.clone())
    }

    /// What the server said about this tool's side effects, if anything.
    ///
    /// **Only safe to act on when it says `false`.** A `readOnlyHint` is the
    /// server's description of itself, and the server is exactly the party the
    /// confirmation exists to protect against: believing `true` would let any
    /// server open the gate by naming its own tool harmless. Believing `false`
    /// only ever closes it, which a server has no reason to lie about - and
    /// catches what the name heuristic cannot, such as a tool called
    /// `list_and_prune`.
    pub fn declared_read_only_for(&self, function_name: &str) -> Option<bool> {
        self.bindings
            .get(function_name)
            .and_then(|binding| binding.declared_read_only)
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
}
