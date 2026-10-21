//! Responses wire conversion. Opaque output items are replayed verbatim, never decoded.
use super::*;
const NAMESPACE: &str = "doge_shell";

/// Per-request wire policy: who the history belongs to and which optional
/// fields the route accepts.
pub(super) struct Wire {
    model: String,
    identity: Value,
    /// `None` on ChatGPT subscription, which rejects sampling, output limits
    /// and cache metadata and takes tools inside the `doge_shell` namespace.
    api_key: Option<ApiKeyFields>,
}
struct ApiKeyFields {
    reasoning_effort: Option<String>,
}
impl Wire {
    pub(super) fn subscription(model: &str, account: &Account) -> Self {
        Self {
            model: model.into(),
            identity: json!({"provider":"chatgpt_subscription", "model":model, "account":account.label, "client_id":account.client_id, "subject":account.subject}),
            api_key: None,
        }
    }
    pub(super) fn api_key(
        model: &str,
        endpoint: &str,
        key_fingerprint: &str,
        reasoning_effort: Option<String>,
    ) -> Self {
        // No `model` here: `chat_model` promises the carried conversation
        // continues with the new model, so a model change only stops the old
        // model's opaque reasoning from being replayed (see `replay`).
        Self {
            model: model.into(),
            identity: json!({"provider":"api_key", "endpoint":endpoint, "key":key_fingerprint}),
            api_key: Some(ApiKeyFields { reasoning_effort }),
        }
    }
    pub(super) fn is_subscription(&self) -> bool {
        self.api_key.is_none()
    }
    /// Subscription namespaces local tools; the API-key route sends them flat,
    /// which every Responses-compatible gateway understands.
    fn namespace(&self) -> Option<&'static str> {
        self.is_subscription().then_some(NAMESPACE)
    }
    /// The output items of one earlier turn to send back.
    ///
    /// Subscription replays verbatim. On the API-key route a reasoning item is
    /// dropped when it carries no encrypted content (the endpoint rejected
    /// `include`, so a stateless request cannot resolve its id) or came from
    /// another model. Once one is dropped, the turn's remaining items lose
    /// their ids too: an id ties a function call to its reasoning item, and
    /// OpenAI rejects a call whose paired item is missing.
    fn replay(&self, continuation: &Value, output: &[Value]) -> Vec<Value> {
        if self.is_subscription() {
            return output.to_vec();
        }
        let same_model = continuation.get("model").and_then(Value::as_str) == Some(&self.model);
        let keep = |item: &&Value| {
            item.get("type").and_then(Value::as_str) != Some("reasoning")
                || (same_model && item.get("encrypted_content").is_some_and(|c| !c.is_null()))
        };
        let kept: Vec<Value> = output.iter().filter(keep).cloned().collect();
        if kept.len() == output.len() {
            return kept;
        }
        kept.into_iter()
            .map(|mut item| {
                if let Some(map) = item.as_object_mut() {
                    map.remove("id");
                }
                item
            })
            .collect()
    }
}
fn functions(options: &ChatRequestOptions) -> Result<HashMap<String, Value>> {
    let mut functions = HashMap::new();
    for tool in options.tools.as_deref().unwrap_or_default() {
        if tool.get("type").and_then(Value::as_str) != Some("function") {
            bail!("Responses supports only registered local function tools.");
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
    wire: &Wire,
) -> Result<Value> {
    let expected = &wire.identity;
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
                let continuation = message.get(CONTINUATION).ok_or_else(|| anyhow!("This history was not created with the Responses API. Run chat_reset before continuing."))?;
                if continuation.get("identity") != Some(expected) {
                    bail!(
                        "Provider, model, endpoint, API key, or account changed. Run chat_reset before continuing this history."
                    );
                }
                let output = continuation
                    .get("output")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow!("Missing Responses-native history. Run chat_reset."))?;
                input.extend(wire.replay(continuation, output));
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
    // Requested on every model: whether a model reasons is not knowable from
    // a deployment name. An endpoint or model that rejects it costs one
    // retry (`recovery`), after which reasoning items are not replayed.
    let mut body = json!({"model":wire.model, "store":false, "stream":true, "input":input, "include":["reasoning.encrypted_content"]});
    if !tools.is_empty() {
        body["tools"] = match wire.namespace() {
            Some(namespace) => {
                json!([{"type":"namespace", "name":namespace, "description":"Available shell assistant tools.", "tools":tools}])
            }
            None => json!(tools),
        };
    }
    if let Some(fields) = &wire.api_key {
        // Reasoning models reject sampling parameters on Responses.
        if let Some(temperature) = options.temperature
            && !crate::is_openai_reasoning_model(&wire.model)
        {
            body["temperature"] = json!(temperature);
        }
        if let Some(max_tokens) = options.max_tokens {
            // Responses rejects a `max_output_tokens` below 16.
            body["max_output_tokens"] = json!(max_tokens.max(16));
        }
        if let Some(key) = &options.prompt_cache_key {
            body["prompt_cache_key"] = json!(key);
        }
        if let Some(effort) = &fields.reasoning_effort {
            body["reasoning"] = json!({"effort":effort});
        }
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
            _ => bail!("Unsupported structured output format for the Responses API."),
        };
    }
    // On subscription, sampling, output limits, cache metadata, and unsupported SIWC fields are deliberately absent.
    Ok(body)
}
pub(super) fn normalize(
    native: &Value,
    options: &ChatRequestOptions,
    wire: &Wire,
) -> Result<Value> {
    // An API-key response cut short by `max_output_tokens` or a content
    // filter is reported like a chat/completions `finish_reason`, so
    // `turn::interpret_response` names the cut. Subscription treats any
    // unfinished response as a failure.
    let cut = match native.get("status").and_then(Value::as_str) {
        Some("completed") => None,
        Some("incomplete") if !wire.is_subscription() => Some(
            match native
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str)
            {
                Some("content_filter") => "content_filter",
                _ => crate::turn::FINISH_REASON_LENGTH,
            },
        ),
        _ => bail!("Responses terminal status is not completed."),
    };
    let mut output = native
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Responses completion has no output array."))?
        .clone();
    if cut.is_some() {
        // A cut-off call may hold half its arguments; never run or replay it.
        output.retain(|item| item.get("type").and_then(Value::as_str) != Some("function_call"));
    }
    let output = &output;
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
                        Some("refusal") => bail!("The model refused this request."),
                        _ => bail!("Unsupported Responses message content."),
                    }
                }
            }
            Some("function_call") => {
                if item.get("namespace").and_then(Value::as_str) != wire.namespace() {
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
    if options.response_format.is_some() && calls.is_empty() && cut.is_none() {
        serde_json::from_str::<Value>(&text)
            .map_err(|_| anyhow!("Completed structured output is invalid JSON."))?;
    }
    let mut continuation = json!({"identity":wire.identity,"output":output});
    if !wire.is_subscription() {
        continuation["model"] = json!(wire.model);
    }
    let mut message = json!({"role":"assistant", "content":text, CONTINUATION:continuation});
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }
    let finish_reason = cut.unwrap_or(if calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    });
    let mut result = json!({"choices":[{"message":message,"finish_reason":finish_reason}]});
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
