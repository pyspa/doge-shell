//! Responses wire conversion. Opaque output items are replayed verbatim, never decoded.
use super::*;
fn identity(model: &str, account: &Account) -> Value {
    json!({"provider":"chatgpt_subscription", "model":model, "account":account.label, "client_id":account.client_id, "subject":account.subject})
}
fn functions(options: &ChatRequestOptions) -> Result<HashMap<String, Value>> {
    let mut functions = HashMap::new();
    for tool in options.tools.as_deref().unwrap_or_default() {
        if tool.get("type").and_then(Value::as_str) != Some("function") {
            bail!("Subscription supports only registered local function tools.");
        }
        let definition = tool
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("Invalid local function definition."))?;
        let name = definition
            .get("name")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow!("Local function has no name."))?;
        let parameters = definition
            .get("parameters")
            .ok_or_else(|| anyhow!("Local function has no parameters."))?;
        let mut converted =
            json!({"type":"function", "name":name, "parameters":parameters, "strict":false});
        if let Some(description) = definition.get("description") {
            converted["description"] = description.clone();
        }
        if functions.insert(name.to_owned(), converted).is_some() {
            bail!("Duplicate local function name.");
        }
    }
    Ok(functions)
}
pub(super) fn build_body(
    messages: &[Value],
    options: &ChatRequestOptions,
    model: &str,
    account: &Account,
) -> Result<Value> {
    let expected = identity(model, account);
    let mut input = Vec::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match role {
            "system" | "developer" | "user" => {
                input.push(json!({"type":"message", "role":if role == "system" {"developer"} else {role}, "content":message.get("content").cloned().unwrap_or(json!(""))}));
            }
            "assistant" => {
                let continuation = message.get(CONTINUATION).ok_or_else(|| anyhow!("This history uses another provider. Run chat_reset before using ChatGPT subscription."))?;
                if continuation.get("identity") != Some(&expected) {
                    bail!(
                        "Provider, model, or ChatGPT account changed. Run chat_reset before continuing this history."
                    );
                }
                let output = continuation
                    .get("output")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow!("Missing Responses-native history. Run chat_reset."))?;
                input.extend(output.iter().cloned());
            }
            "tool" => {
                let call_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| anyhow!("Tool result has no call ID."))?;
                let output = message
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Tool result must be a string."))?;
                input.push(
                    json!({"type":"function_call_output", "call_id":call_id, "output":output}),
                );
            }
            _ => bail!("Unsupported history item role."),
        }
    }
    let converted = functions(options)?;
    // Maintain tool registration order rather than HashMap iteration order.
    let tools: Vec<Value> = options
        .tools
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|tool| {
            tool.pointer("/function/name")
                .and_then(Value::as_str)
                .and_then(|name| converted.get(name))
                .cloned()
        })
        .collect();
    let mut body = json!({"model":model, "store":false, "stream":true, "input":input, "include":["reasoning.encrypted_content"]});
    if !tools.is_empty() {
        body["tools"] = json!([{"type":"namespace", "name":"doge_shell", "description":"Available shell assistant tools.", "tools":tools}]);
    }
    if let Some(format) = &options.response_format {
        body["text"] = match format.get("type").and_then(Value::as_str) {
            Some("json_schema") => {
                let schema = format
                    .get("json_schema")
                    .and_then(Value::as_object)
                    .ok_or_else(|| anyhow!("Invalid structured JSON schema format."))?;
                let mut flattened = Value::Object(schema.clone());
                flattened["type"] = json!("json_schema");
                json!({"format":flattened})
            }
            Some("json_object") => {
                body["input"].as_array_mut().unwrap().insert(0, json!({"type":"message","role":"developer","content":"Return a valid JSON object."}));
                json!({"format":{"type":"json_object"}})
            }
            _ => bail!("Unsupported structured output format for ChatGPT subscription."),
        };
    }
    // Sampling, output limits, cache metadata, and unsupported SIWC fields are deliberately absent.
    Ok(body)
}
pub(super) fn normalize(
    native: &Value,
    options: &ChatRequestOptions,
    model: &str,
    account: &Account,
) -> Result<Value> {
    if native.get("status").and_then(Value::as_str) != Some("completed") {
        bail!("Responses terminal status is not completed.");
    }
    let output = native
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Responses completion has no output array."))?;
    let allowlist = functions(options)?;
    let mut call_ids = HashSet::new();
    let mut calls = Vec::new();
    let mut text = String::new();
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("reasoning") => {}
            Some("message") => {
                if item.get("role").and_then(Value::as_str) != Some("assistant") {
                    bail!("Unexpected Responses message role.");
                }
                for content in item
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow!("Responses message has no content array."))?
                {
                    match content.get("type").and_then(Value::as_str) {
                        Some("output_text") => text.push_str(
                            content
                                .get("text")
                                .and_then(Value::as_str)
                                .ok_or_else(|| anyhow!("Invalid Responses text content."))?,
                        ),
                        Some("refusal") => bail!("ChatGPT refused this request."),
                        _ => bail!("Unsupported Responses message content."),
                    }
                }
            }
            Some("function_call") => {
                if item.get("namespace").and_then(Value::as_str) != Some("doge_shell") {
                    bail!("Unrecognized tool namespace; tool execution was refused.");
                }
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Function call has no name."))?;
                let schema = allowlist.get(name).ok_or_else(|| {
                    anyhow!("Unregistered function call; tool execution was refused.")
                })?;
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow!("Function call has no call ID."))?;
                if !call_ids.insert(call_id) {
                    bail!("Duplicate function call ID; tool execution was refused.");
                }
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Function call has no arguments."))?;
                let args: Value = serde_json::from_str(arguments).map_err(|_| {
                    anyhow!("Invalid JSON function arguments; tool execution was refused.")
                })?;
                if !args.is_object() {
                    bail!("Function arguments must be an object.");
                }
                let validator = jsonschema::validator_for(&schema["parameters"]).map_err(|_| {
                    anyhow!(
                        "Local function schema cannot be validated; tool execution was refused."
                    )
                })?;
                if !validator.is_valid(&args) {
                    bail!(
                        "Function arguments do not match the registered schema; tool execution was refused."
                    );
                }
                calls.push(json!({"id":call_id, "type":"function", "function":{"name":name,"arguments":arguments}}));
            }
            _ => bail!("Unsupported Responses output tool; execution was refused."),
        }
    }
    if options.response_format.is_some() && calls.is_empty() {
        serde_json::from_str::<Value>(&text)
            .map_err(|_| anyhow!("Completed ChatGPT structured output is invalid JSON."))?;
    }
    let mut message = json!({"role":"assistant", "content":text, CONTINUATION:{"identity":identity(model,account),"output":output}});
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }
    let mut result = json!({"choices":[{"message":message,"finish_reason":if calls.is_empty(){"stop"}else{"tool_calls"}}]});
    if let Some(usage) = normalized_usage(native) {
        result["usage"] = usage;
    }
    Ok(result)
}
pub(super) fn normalized_usage(native: &Value) -> Option<Value> {
    if let Some(usage) = native.get("usage")
        && usage.get("input_tokens").is_some_and(Value::is_u64)
        && usage.get("output_tokens").is_some_and(Value::is_u64)
    {
        let mut normalized = serde_json::Map::new();
        for (source, target) in [
            ("input_tokens", "prompt_tokens"),
            ("output_tokens", "completion_tokens"),
        ] {
            if let Some(value) = usage.get(source).filter(|v| v.is_u64()) {
                normalized.insert(target.into(), value.clone());
            }
        }
        if let Some(cached) = usage
            .pointer("/input_tokens_details/cached_tokens")
            .filter(|v| v.is_u64())
        {
            normalized.insert(
                "prompt_tokens_details".into(),
                json!({"cached_tokens":cached}),
            );
        }
        return Some(Value::Object(normalized));
    }
    None
}
