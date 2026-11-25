//! A trait boundary around sending a chat request.
//!
//! `dsh-builtin` cannot depend on `dsh` (see the workspace's crate graph),
//! so its AI chat loop (`dsh-builtin/src/chatgpt.rs::chat_with_tools`) used
//! to take a concrete `&ChatGptClient` and could not be exercised without a
//! real provider. This trait lives here - the one crate both `dsh` and
//! `dsh-builtin` can depend on - so either side can accept `&dyn ChatClient`
//! and hand it a scripted double in tests.

use crate::client::{ChatGptClient, ChatRequestOptions};
use anyhow::Result;
use serde_json::Value;

/// Chat client trait for sending requests to chat APIs.
pub trait ChatClient: Send + Sync {
    /// Opaque provider/account identity for local answer caches; not a credential.
    fn cache_scope(&self) -> String {
        String::new()
    }
    /// Identity allowed to receive this conversation's history.
    fn conversation_scope(&self) -> String {
        self.cache_scope()
    }

    /// Recognize older on-disk bindings when a transport's history policy changed.
    fn accepts_legacy_conversation_scope(&self, _scope: &str, _messages: &[Value]) -> bool {
        false
    }

    fn send_chat_cancellable(
        &self,
        messages: &[Value],
        options: &ChatRequestOptions,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Value> {
        if cancel() {
            anyhow::bail!("AI request cancelled");
        }
        self.send_chat_request(messages, options)
    }
    /// Send a chat request.
    fn send_chat_request(&self, messages: &[Value], options: &ChatRequestOptions) -> Result<Value>;

    /// Send a chat request with the reply delivered incrementally.
    ///
    /// Defaulted to one non-streaming call whose full answer is delivered as
    /// a single `on_delta` chunk. Real incremental streaming depends on the
    /// transport underneath, so a test double (or a future non-HTTP client)
    /// has no reason to reimplement chunking just to satisfy this trait;
    /// `ChatGptClient` overrides this with real server-sent-event streaming.
    fn send_chat_streaming(
        &self,
        messages: &[Value],
        options: &ChatRequestOptions,
        cancel: &dyn Fn() -> bool,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Value> {
        let response = self.send_chat_cancellable(messages, options, cancel)?;
        if let Some(text) = response
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("message"))
            .and_then(crate::turn::extract_message_content)
        {
            on_delta(&text);
        }
        Ok(response)
    }
}

impl ChatClient for ChatGptClient {
    fn cache_scope(&self) -> String {
        ChatGptClient::cache_scope(self)
    }
    fn conversation_scope(&self) -> String {
        if self.uses_api_key_responses() {
            format!("responses:{}", self.cache_scope_for_model(""))
        } else {
            self.cache_scope()
        }
    }

    fn accepts_legacy_conversation_scope(&self, scope: &str, messages: &[Value]) -> bool {
        // Older files used a model-bound cache key. Verify it against the same
        // endpoint and key before upgrading, even when the model just changed.
        self.uses_api_key_responses()
            && (scope == self.cache_scope()
                || messages
                    .iter()
                    .filter_map(|message| {
                        message
                            .get(crate::responses::CONTINUATION)?
                            .get("model")?
                            .as_str()
                    })
                    .any(|model| self.cache_scope_for_model(model) == scope))
    }

    fn send_chat_cancellable(
        &self,
        messages: &[Value],
        options: &ChatRequestOptions,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Value> {
        self.send_chat(messages, options, Some(cancel))
    }
    fn send_chat_request(&self, messages: &[Value], options: &ChatRequestOptions) -> Result<Value> {
        self.send_chat(messages, options, None)
    }
    fn send_chat_streaming(
        &self,
        messages: &[Value],
        options: &ChatRequestOptions,
        cancel: &dyn Fn() -> bool,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Value> {
        ChatGptClient::send_chat_streaming(self, messages, options, Some(cancel), on_delta)
    }
}
