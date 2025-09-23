use serde_json::Value;

mod edit;
mod execute;

pub fn build_tools() -> Vec<Value> {
    vec![edit::definition(), execute::definition()]
}

pub fn execute_tool_call(tool_call: &Value) -> Result<String, String> {
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

    match name {
        edit::NAME => edit::run(arguments),
        execute::NAME => execute::run(arguments),
        other => Err(format!("chat: unsupported tool `{other}`")),
    }
}
