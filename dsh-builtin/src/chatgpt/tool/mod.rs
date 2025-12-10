use serde_json::Value;

use super::mcp::McpManager;
use crate::ShellProxy;

mod edit;
mod execute;
mod ls;
mod read;
mod search;

const MAX_OUTPUT_LENGTH: usize = 4096;

pub fn build_tools() -> Vec<Value> {
    vec![
        edit::definition(),
        execute::definition(),
        ls::definition(),
        read::definition(),
        search::definition(),
    ]
}

pub fn execute_tool_call(
    tool_call: &Value,
    mcp: &McpManager,
    proxy: &mut dyn ShellProxy,
) -> Result<String, String> {
    let function = tool_call
        .get("function")
        .ok_or_else(|| "chat: tool call missing function".to_string())?;

    let name = function
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: tool call missing function name".to_string())?;

    let arguments = function
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    let result = if let Some(result) = mcp.execute_tool(name, arguments)? {
        result
    } else {
        match name {
            edit::NAME => edit::run(arguments, proxy)?,
            execute::NAME => execute::run(arguments, proxy)?,
            ls::NAME => ls::run(arguments, proxy)?,
            read::NAME => read::run(arguments, proxy)?,
            search::NAME => search::run(arguments, proxy)?,
            other => return Err(format!("chat: unsupported tool `{other}`")),
        }
    };

    Ok(truncate_output(result))
}

fn truncate_output(output: String) -> String {
    if output.len() > MAX_OUTPUT_LENGTH {
        let truncated = &output[..MAX_OUTPUT_LENGTH];
        let omitted = output.len() - MAX_OUTPUT_LENGTH;
        format!("{truncated}\n... (truncated {omitted} characters)")
    } else {
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::Context;

    struct NoopProxy;
    impl ShellProxy for NoopProxy {
        fn get_current_dir(&self) -> anyhow::Result<std::path::PathBuf> {
            Ok(std::env::current_dir()?)
        }
        fn exit_shell(&mut self) {}
        fn dispatch(
            &mut self,
            _ctx: &Context,
            _cmd: &str,
            _argv: Vec<String>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        fn save_path_history(&mut self, _path: &str) {}
        fn changepwd(&mut self, _path: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn insert_path(&mut self, _index: usize, _path: &str) {}
        fn get_var(&mut self, _key: &str) -> Option<String> {
            None
        }
        fn set_var(&mut self, _key: String, _value: String) {}
        fn set_env_var(&mut self, _key: String, _value: String) {}
        fn get_alias(&mut self, _name: &str) -> Option<String> {
            None
        }
        fn set_alias(&mut self, _name: String, _command: String) {}
        fn list_aliases(&mut self) -> std::collections::HashMap<String, String> {
            std::collections::HashMap::new()
        }
        fn add_abbr(&mut self, _name: String, _expansion: String) {}
        fn remove_abbr(&mut self, _name: &str) -> bool {
            false
        }
        fn list_abbrs(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn get_abbr(&self, _name: &str) -> Option<String> {
            None
        }
        fn list_mcp_servers(&mut self) -> Vec<dsh_types::mcp::McpServerConfig> {
            Vec::new()
        }
        fn list_execute_allowlist(&mut self) -> Vec<String> {
            Vec::new()
        }
        fn list_exported_vars(&self) -> Vec<(String, String)> {
            vec![]
        }
        fn export_var(&mut self, _key: &str) -> bool {
            true
        }
        fn set_and_export_var(&mut self, _key: String, _value: String) {}
    }

    #[test]
    fn test_truncation_short() {
        let short = "Short output";
        assert_eq!(truncate_output(short.to_string()), short);
    }

    #[test]
    fn test_truncation_exact() {
        let exact = "a".repeat(MAX_OUTPUT_LENGTH);
        assert_eq!(truncate_output(exact.clone()), exact);
    }

    #[test]
    fn test_truncation_long() {
        let long = "a".repeat(MAX_OUTPUT_LENGTH + 10);
        let truncated = truncate_output(long);
        assert!(truncated.contains("... (truncated 10 characters)"));
        assert_eq!(truncated.len(), MAX_OUTPUT_LENGTH + 30); // 30 is length of "\n... (truncated 10 characters)" approx
    }

    #[test]
    fn test_execute_tool_call_unknown_tool() {
        let mut proxy = NoopProxy;
        let mcp = McpManager::load(vec![]);
        let tool_call = serde_json::json!({
            "function": {
                "name": "unknown_tool",
                "arguments": "{}"
            }
        });

        let result = execute_tool_call(&tool_call, &mcp, &mut proxy);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "chat: unsupported tool `unknown_tool`");
    }
}
