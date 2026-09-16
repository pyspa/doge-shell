//! The `chat_*` builtin commands (`chat_prompt`/`chat_model`/`chat_reset`/
//! `chat_status`) and `execute_chat_message`, the synchronous entry point `!`
//! itself calls.
use super::*;
use crate::markdown::render_markdown_with_fallback;

pub fn load_openai_config(proxy: &mut dyn ShellProxy) -> OpenAiConfig {
    OpenAiConfig::from_getter(|key| proxy.get_var(key).or_else(|| std::env::var(key).ok()))
}

/// Execute a chat request using the configured OpenAI client
pub fn execute_chat_message(
    ctx: &Context,
    proxy: &mut dyn ChatToolHost,
    message: &str,
    model_override: Option<&str>,
) -> ExitStatus {
    if message.trim().is_empty() {
        ctx.write_stderr("chat: message content required").ok();
        return ExitStatus::ExitedWith(1);
    }

    let config = load_openai_config(proxy);

    if config.api_key().is_none() {
        ctx.write_stderr(&format!(
            "chat: AI service is not configured. {}",
            dsh_openai::API_KEY_SETUP_HINT
        ))
        .ok();
        return ExitStatus::ExitedWith(1);
    }

    match ChatGptClient::try_from_config(&config) {
        Ok(client) => {
            let prompt = proxy.get_var(PROMPT_KEY);
            let language = proxy.get_var(LANGUAGE_KEY);
            let model_override = model_override.map(|model| model.to_string());
            // The shell's own manager, so `mcp connect` / `mcp disconnect` /
            // `mcp status` and the agent describe the same connections.
            let mcp_manager = proxy.agent_mcp_manager();

            let stream_enabled = resolve_stream_enabled(proxy);
            let mut sink = stream_enabled.then(|| StreamSink::new(ctx));

            match chat_with_tools(
                &client,
                message,
                prompt,
                language,
                Some(0.1),
                model_override,
                &mcp_manager,
                sink.as_mut(),
                proxy,
            ) {
                Ok(res) => {
                    // Already on the screen: streaming rendered this same
                    // text (the final iteration's content, unchanged) block
                    // by block as it arrived. This must be the *last*
                    // iteration's own flag, not "did any earlier iteration
                    // stream something" - a per-request fallback can leave
                    // an interim round streamed but the round that produced
                    // `res` un-streamed, and `wrote_any` alone would then
                    // skip printing the answer entirely.
                    let already_shown = sink
                        .as_ref()
                        .is_some_and(StreamSink::streamed_this_iteration);
                    if !already_shown {
                        let rendered = render_markdown_with_fallback(res.trim());
                        let trimmed = rendered.trim_end_matches('\n');
                        ctx.write_stdout(trimmed).ok();
                    }
                    ExitStatus::ExitedWith(0)
                }
                Err(err) if err == CANCELLED_MESSAGE => ExitStatus::ExitedWith(1),
                Err(err) => {
                    ctx.write_stderr(&format!("\r{err:?}")).ok();
                    ExitStatus::ExitedWith(1)
                }
            }
        }
        Err(err) => {
            ctx.write_stderr(&format!("\r{err:?}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Built-in chat_prompt command description
pub fn chat_prompt_description() -> &'static str {
    "Set or show the system prompt for chat"
}

/// Built-in chat_prompt command implementation
/// Sets a custom prompt template for ChatGPT interactions
/// The prompt template is used to provide context for all subsequent chat commands
pub fn chat_prompt(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 2 {
        // Require prompt text as argument
        ctx.write_stderr("Usage: chat_prompt <prompt_text>").ok();
        ExitStatus::ExitedWith(1)
    } else {
        let prompt = &argv[1];
        // Store the prompt template in shell variables
        proxy.set_var(PROMPT_KEY.to_string(), prompt.to_string());
        ctx.write_stdout(&format!("Chat prompt set to: {prompt}"))
            .ok();
        ExitStatus::ExitedWith(0)
    }
}

/// Built-in chat_model command description
pub fn chat_model_description() -> &'static str {
    "Set or show the AI model used for chat"
}

/// Built-in chat_model command implementation
/// Manages the default OpenAI model for ChatGPT interactions
///
/// Usage:
///   chat_model                - Show current default model
///   chat_model <model>        - Set default model, effective for every AI
///                               path (`!` chat, ghost text, command palette
///                               actions, `ai-watch`, `blocks explain|fix`,
///                               ...) as soon as it is set - no restart needed
///   chat_model ""             - Clear the override and fall back to the
///                               provider's default model
pub fn chat_model(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match argv.len() {
        1 => {
            // Show current model using resolved configuration
            let config = load_openai_config(proxy);
            let current_model = config.default_model().to_string();
            ctx.write_stdout(&format!("Current OpenAI model: {current_model}"))
                .ok();
            ExitStatus::ExitedWith(0)
        }
        2 => {
            // Set new model. An empty (or whitespace-only) value clears the
            // override, falling back to the provider's default model. Trim
            // once and store the trimmed value, so `$AI_CHAT_MODEL` itself
            // never ends up holding stray whitespace that every reader has
            // to trim around again.
            let new_model = argv[1].trim();
            proxy.set_var(MODEL_KEY.to_string(), new_model.to_string());
            if new_model.is_empty() {
                let config = load_openai_config(proxy);
                ctx.write_stdout(&format!(
                    "OpenAI model reset to default: {}",
                    config.default_model()
                ))
                .ok();
            } else {
                ctx.write_stdout(&format!("OpenAI model set to: {new_model}"))
                    .ok();
            }
            ExitStatus::ExitedWith(0)
        }
        _ => {
            ctx.write_stderr("Usage: chat_model [model_name]").ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Built-in chat_reset command description
pub fn chat_reset_description() -> &'static str {
    "Forget the carried AI chat conversation"
}

/// Built-in chat_reset command implementation
///
/// Consecutive `!` turns continue the same conversation; this starts over.
pub fn chat_reset(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() > 1 {
        ctx.write_stderr("Usage: chat_reset").ok();
        return ExitStatus::ExitedWith(1);
    }

    let ttl = resolve_session_ttl(proxy);
    let detail = session::session_description(ttl);
    let cleared = session::session_reset();

    let message = match (cleared, detail) {
        (true, Some(detail)) => format!("chat session cleared ({detail})"),
        (true, None) => "chat session cleared".to_string(),
        (false, _) => "no chat session to clear".to_string(),
    };
    ctx.write_stdout(&message).ok();
    ExitStatus::ExitedWith(0)
}

/// Built-in chat_status command description
pub fn chat_status_description() -> &'static str {
    "Show the carried AI chat conversation"
}

/// Built-in chat_status command implementation
///
/// Read-only counterpart to `chat_reset`: names the conversation a follow-up
/// `!` would continue, without discarding it - based on idle time, though.
/// `session_description` cannot re-check whether the operator prompt,
/// language, MCP connections or project changed since the conversation was
/// stored (that needs `ChatToolHost`, not the base `ShellProxy` every builtin
/// gets), so a conversation shown here as carried can still turn out to start
/// fresh for one of those reasons.
pub fn chat_status(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() > 1 {
        ctx.write_stderr("Usage: chat_status").ok();
        return ExitStatus::ExitedWith(1);
    }

    let ttl = resolve_session_ttl(proxy);
    let message = match session::session_description(ttl) {
        Some(detail) => format!("chat session {detail}"),
        None if ttl.is_none() => format!(
            "no chat session ({} is 0, so `!` turns do not share a conversation)",
            session::SESSION_TTL_KEY
        ),
        None => "no chat session carried".to_string(),
    };
    ctx.write_stdout(&message).ok();
    ExitStatus::ExitedWith(0)
}

/// Describe the carried conversation, for `doctor ai`.
pub fn chat_session_description(proxy: &mut dyn ShellProxy) -> Option<String> {
    session::session_description(resolve_session_ttl(proxy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;
    use nix::unistd::getpid;

    fn test_ctx() -> Context {
        let pid = getpid();
        Context::new_safe(pid, pid, false)
    }

    #[test]
    fn chat_model_sets_the_shell_variable() {
        let mut proxy = TestShellProxy::default();
        let ctx = test_ctx();

        let result = chat_model(
            &ctx,
            vec!["chat_model".to_string(), "gpt-4o-mini".to_string()],
            &mut proxy,
        );

        assert_eq!(result, ExitStatus::ExitedWith(0));
        assert_eq!(proxy.vars.get(MODEL_KEY), Some(&"gpt-4o-mini".to_string()));
    }

    /// `chat_model ""` clears the override. `Environment::reload_chat_model`
    /// (the live path) and `OpenAiConfig` both treat a blank value as unset,
    /// so this only has to check the variable itself lands empty - the
    /// resolved-model display already goes through `load_openai_config`.
    #[test]
    fn chat_model_with_an_empty_argument_clears_the_override() {
        let mut proxy = TestShellProxy::default();
        proxy
            .vars
            .insert(MODEL_KEY.to_string(), "gpt-4o".to_string());
        let ctx = test_ctx();

        let result = chat_model(
            &ctx,
            vec!["chat_model".to_string(), "".to_string()],
            &mut proxy,
        );

        assert_eq!(result, ExitStatus::ExitedWith(0));
        assert_eq!(proxy.vars.get(MODEL_KEY), Some(&String::new()));
    }

    #[test]
    fn chat_model_rejects_extra_arguments() {
        let mut proxy = TestShellProxy::default();
        let ctx = test_ctx();

        let result = chat_model(
            &ctx,
            vec!["chat_model".to_string(), "a".to_string(), "b".to_string()],
            &mut proxy,
        );

        assert_eq!(result, ExitStatus::ExitedWith(1));
    }
}
