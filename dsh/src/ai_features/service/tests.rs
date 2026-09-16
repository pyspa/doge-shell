use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Answers every request with a reply that has neither tool calls nor
/// content - the shape that used to be resent until the iteration cap.
struct StallingClient {
    calls: Arc<AtomicUsize>,
}

impl ChatClient for StallingClient {
    fn send_chat_request(
        &self,
        _messages: &[Value],
        _options: &ChatRequestOptions,
    ) -> Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({
            "choices": [{ "message": { "role": "assistant" }, "finish_reason": "tool_calls" }]
        }))
    }
}

struct EchoClient;

impl ChatClient for EchoClient {
    fn send_chat_request(
        &self,
        _messages: &[Value],
        options: &ChatRequestOptions,
    ) -> Result<Value> {
        Ok(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": format!(
                        "cap={:?} json={}",
                        options.max_tokens,
                        options.response_format.is_some()
                    )
                },
                "finish_reason": "stop"
            }]
        }))
    }
}

/// Echoes the requested model back, so a test can see what was actually sent.
struct ModelEchoClient;

impl ChatClient for ModelEchoClient {
    fn send_chat_request(
        &self,
        _messages: &[Value],
        options: &ChatRequestOptions,
    ) -> Result<Value> {
        Ok(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": format!("model={:?}", options.model)
                },
                "finish_reason": "stop"
            }]
        }))
    }
}

fn test_policy(level: SafetyLevel) -> AgentPolicyHandles {
    AgentPolicyHandles {
        safety_level: Arc::new(RwLock::new(level)),
        safety_guard: Arc::new(SafetyGuard::new()),
        execute_allowlist: Arc::new(RwLock::new(Vec::new())),
        agent_session_allowlist: Arc::new(RwLock::new(Vec::new())),
    }
}

fn service(client: impl ChatClient + 'static) -> LiveAiService {
    LiveAiService::new(
        client,
        Arc::new(RwLock::new(McpManager::default())),
        test_policy(SafetyLevel::Normal),
        None,
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(None)),
    )
}

fn service_with_model(client: impl ChatClient + 'static, model: Option<&str>) -> LiveAiService {
    LiveAiService::new(
        client,
        Arc::new(RwLock::new(McpManager::default())),
        test_policy(SafetyLevel::Normal),
        None,
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(model.map(str::to_string))),
    )
}

#[test]
fn tools_require_explicit_opt_in() {
    let tools = Some(vec![json!({"type": "function"})]);

    let default_options = AiRequestOptions::new(Some(0.1)).to_chat_options(tools.clone());
    assert!(default_options.tools.is_none());

    let enabled_options = AiRequestOptions::new(Some(0.1))
        .with_tools()
        .to_chat_options(tools);
    assert!(enabled_options.tools.is_some());
}

#[tokio::test]
async fn a_stalling_model_stops_after_one_nudge() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = service(StallingClient {
        calls: calls.clone(),
    });

    let result = service
        .send_request(vec![json!({"role": "user", "content": "hi"})], Some(0.1))
        .await;

    assert!(result.is_err());
    // One request, one nudged retry - not the 10-iteration cap.
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_bounded_request_forwards_its_token_cap() {
    let service = service(EchoClient);

    let answer = service
        .send_request_with(
            vec![json!({"role": "user", "content": "hi"})],
            AiRequestOptions::new(Some(0.1))
                .with_max_tokens(Some(64))
                .as_json_object(),
        )
        .await
        .unwrap();

    assert_eq!(answer, "cap=Some(64) json=true");
}

/// `chat_model` (via `Environment::reload_chat_model`) has to reach every AI
/// request, not just the `!` runtime - this is `LiveAiService`'s half of that:
/// whatever is in the shared slot at request time overrides the client's own
/// default model, and an unset slot leaves the client's default alone.
#[tokio::test]
async fn a_configured_model_overrides_the_clients_default() {
    let service = service_with_model(ModelEchoClient, Some("gpt-4o-mini"));

    let answer = service
        .send_request(vec![json!({"role": "user", "content": "hi"})], Some(0.1))
        .await
        .unwrap();

    assert_eq!(answer, "model=Some(\"gpt-4o-mini\")");
}

#[tokio::test]
async fn an_unset_model_slot_leaves_the_clients_default_alone() {
    let service = service_with_model(ModelEchoClient, None);

    let answer = service
        .send_request(vec![json!({"role": "user", "content": "hi"})], Some(0.1))
        .await
        .unwrap();

    assert_eq!(answer, "model=None");
}

fn service_speaking(client: impl ChatClient + 'static, language: &str) -> LiveAiService {
    LiveAiService::new(
        client,
        Arc::new(RwLock::new(McpManager::default())),
        test_policy(SafetyLevel::Normal),
        None,
        Arc::new(RwLock::new(Some(language.to_string()))),
        Arc::new(RwLock::new(None)),
    )
}

/// Echoes the system message back, so a test can see what was sent.
struct SystemEchoClient;

impl ChatClient for SystemEchoClient {
    fn send_chat_request(
        &self,
        messages: &[Value],
        _options: &ChatRequestOptions,
    ) -> Result<Value> {
        let system = messages
            .iter()
            .find(|m| m["role"] == "system")
            .and_then(|m| m["content"].as_str())
            .unwrap_or("<none>")
            .to_string();
        Ok(json!({
            "choices": [{"message": {"role": "assistant", "content": system},
                         "finish_reason": "stop"}]
        }))
    }
}

/// `AI_MESSAGE_LANG` used to reach the `!` runtime only, so every
/// shell-side answer stayed in English however it was set.
#[tokio::test]
async fn the_response_language_reaches_a_shell_side_request() {
    let answer = service_speaking(SystemEchoClient, "Japanese")
        .send_request(
            vec![
                json!({"role": "system", "content": "You explain commands."}),
                json!({"role": "user", "content": "ls"}),
            ],
            Some(0.1),
        )
        .await
        .unwrap();

    assert!(answer.starts_with("You explain commands."));
    assert!(answer.contains("You MUST respond in Japanese."));
}

/// A parsed answer keeps its shape: the caller reads fields, not prose.
#[tokio::test]
async fn a_json_request_is_left_in_its_own_language() {
    let answer = service_speaking(SystemEchoClient, "Japanese")
        .send_request_with(
            vec![
                json!({"role": "system", "content": "Reply with JSON."}),
                json!({"role": "user", "content": "ls"}),
            ],
            AiRequestOptions::new(Some(0.1)).as_json_object(),
        )
        .await
        .unwrap();

    assert_eq!(answer, "Reply with JSON.");
}

#[tokio::test]
async fn an_unbounded_request_sends_no_cap() {
    let service = service(EchoClient);

    let answer = service
        .send_request(vec![json!({"role": "user", "content": "hi"})], Some(0.1))
        .await
        .unwrap();

    assert_eq!(answer, "cap=None json=false");
}

/// Answers one tool call, then stops.
struct OneToolCallClient {
    calls: Arc<AtomicUsize>,
}

struct UnknownToolClient {
    calls: Arc<AtomicUsize>,
    tool_result: Arc<RwLock<Option<String>>>,
}

impl ChatClient for UnknownToolClient {
    fn send_chat_request(
        &self,
        messages: &[Value],
        _options: &ChatRequestOptions,
    ) -> Result<Value> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        if first {
            return Ok(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "ghost",
                            "function": {"name": "mcp__ghost__missing", "arguments": "{}"}
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            }));
        }

        *self.tool_result.write() = messages
            .iter()
            .rev()
            .find(|message| message["role"] == "tool")
            .and_then(|message| message["content"].as_str())
            .map(str::to_string);
        Ok(json!({
            "choices": [{"message": {"role": "assistant", "content": "done"},
                         "finish_reason": "stop"}]
        }))
    }
}

#[tokio::test]
async fn an_unknown_mcp_tool_is_reported_as_an_error() {
    let tool_result = Arc::new(RwLock::new(None));
    let service = LiveAiService::new(
        UnknownToolClient {
            calls: Arc::new(AtomicUsize::new(0)),
            tool_result: tool_result.clone(),
        },
        Arc::new(RwLock::new(McpManager::default())),
        test_policy(SafetyLevel::Normal),
        None,
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(None)),
    );

    service
        .send_request_with(
            vec![json!({"role": "user", "content": "call it"})],
            AiRequestOptions::new(Some(0.1)).with_tools(),
        )
        .await
        .unwrap();

    let result = tool_result.read().clone().expect("tool result was sent");
    assert!(result.contains("Error executing tool"), "{result}");
    assert!(result.contains("was not found"), "{result}");
    assert!(!result.contains("successfully"), "{result}");
}

impl ChatClient for OneToolCallClient {
    fn send_chat_request(
        &self,
        _messages: &[Value],
        _options: &ChatRequestOptions,
    ) -> Result<Value> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        if first {
            Ok(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "1",
                            "function": {"name": "mcp__ops__deploy", "arguments": "{}"}
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            }))
        } else {
            Ok(json!({
                "choices": [{"message": {"role": "assistant", "content": "done"},
                             "finish_reason": "stop"}]
            }))
        }
    }
}

#[tokio::test]
async fn a_default_request_rejects_an_unoffered_tool_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = service(OneToolCallClient {
        calls: calls.clone(),
    });

    let err = service
        .send_request(
            vec![json!({"role": "user", "content": "do not call tools"})],
            Some(0.1),
        )
        .await
        .expect_err("tools are opt-in");

    assert!(err.to_string().contains("does not allow tools"), "{err}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

struct AlwaysAllowHandler;

#[async_trait]
impl ConfirmationHandler for AlwaysAllowHandler {
    async fn confirm(&self, _message: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::AlwaysAllow)
    }
}

/// "always" is a session answer, not a line in the operator's config.
///
/// This used to push the entry into `execute_allowlist` - the list
/// `(chat-execute-add ...)` writes and `list_execute_allowlist` shows - so
/// the two agent loops kept their session approvals in different boxes.
#[tokio::test]
async fn an_always_answer_lands_in_the_session_store() {
    let policy = test_policy(SafetyLevel::Strict);
    let configured = policy.execute_allowlist.clone();
    let session = policy.agent_session_allowlist.clone();

    let service = LiveAiService::new(
        OneToolCallClient {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        Arc::new(RwLock::new(McpManager::default())),
        policy,
        Some(Arc::new(AlwaysAllowHandler)),
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(None)),
    );

    assert!(
        service
            .authorize_mcp_tool("mcp__ops__deploy", "deploy", "{}")
            .await
            .unwrap()
            .is_none()
    );

    assert!(
        configured.read().is_empty(),
        "the operator's list must not collect session answers: {:?}",
        configured.read()
    );
    assert_eq!(session.read().len(), 1);
    assert!(session.read()[0].starts_with("mcp:mcp__ops__deploy"));
}
