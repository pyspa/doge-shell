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

    // A typo here silently becomes the default (see `resolve_ttl`), so say
    // so while there is still a chance to fix it. Checked every turn because
    // the value is resolved every turn.
    if let Some(notice) = invalid_ttl_notice(proxy) {
        ctx.write_stderr(&notice).ok();
    }

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
            let carried = carried_model_note(proxy);
            if new_model.is_empty() {
                let config = load_openai_config(proxy);
                ctx.write_stdout(&format!(
                    "OpenAI model reset to default: {}{carried}",
                    config.default_model()
                ))
                .ok();
            } else {
                ctx.write_stdout(&format!("OpenAI model set to: {new_model}{carried}"))
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
    // Forgetting the conversation orphans anything it started: the ids the
    // model would have polled go with it.
    let cancelled = jobs::cancel_all();

    let mut message = match (cleared, detail) {
        (true, Some(detail)) => format!("chat session cleared ({detail})"),
        (true, None) => "chat session cleared".to_string(),
        (false, _) => "no chat session to clear".to_string(),
    };
    if cancelled > 0 {
        message.push_str(&format!(" ({cancelled} job(s) cancelled)"));
    }
    ctx.write_stdout(&message).ok();
    ExitStatus::ExitedWith(0)
}

/// Built-in chat_status command description
pub fn chat_status_description() -> &'static str {
    "Show the carried AI chat conversation"
}

/// Built-in chat_status command implementation
///
/// Thin compatibility wrapper: the real work needs the MCP manager, which
/// only `ChatToolHost` reaches, so this hands off to the shell core like
/// `cron` does and [`chat_status_detailed`] runs there.
pub fn chat_status(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match proxy.dispatch_core_action(ctx, crate::CoreShellAction::ChatStatus, argv) {
        Ok(()) => ExitStatus::ExitedWith(0),
        Err(error) => {
            let _ = ctx.write_stderr(&format!("chat_status: {error}"));
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Full `chat_status`: answers "would a follow-up `!` continue this?" with
/// the same age/identity/scope check a turn itself runs, instead of the
/// idle-time-only approximation `session_description` can give through a
/// plain `ShellProxy`.
pub fn chat_status_detailed(
    ctx: &Context,
    argv: Vec<String>,
    proxy: &mut dyn ChatToolHost,
) -> anyhow::Result<()> {
    if argv.len() > 1 {
        anyhow::bail!("Usage: chat_status");
    }

    let report = status_report(proxy);
    ctx.write_stdout(&report.status)?;

    // Jobs are reported whether or not a conversation is carried: a running
    // process group is worth naming even when the turn that started it is
    // gone, since `chat_reset` is how a person stops it.
    if !report.jobs.is_empty() {
        ctx.write_stdout(&format!("{} job(s) running:", report.jobs.len()))?;
        for line in &report.jobs {
            ctx.write_stdout(&format!("  {line}"))?;
        }
        ctx.write_stdout("  stop them with chat_reset, or kill -- -<pid> for one")?;
    }
    Ok(())
}

/// The `chat_status` text without the writing: the continuity verdict plus
/// the running-job lines. Split out so tests can assert the verdict without
/// a capturable `Context`.
pub(super) struct StatusReport {
    pub(super) status: String,
    pub(super) jobs: Vec<String>,
}

pub(super) fn status_report(proxy: &mut dyn ChatToolHost) -> StatusReport {
    let ttl = resolve_session_ttl(proxy);
    // Rebuilt exactly the way a turn builds it: the continuity-relevant
    // part of the system prompt (operator prompt, language, MCP fragment),
    // never the skills list, which is re-rendered every turn.
    let identity = {
        let manager = proxy.agent_mcp_manager();
        let manager = manager.read();
        assemble_system_prompt(
            "",
            proxy.get_var(PROMPT_KEY).as_deref(),
            proxy.get_var(LANGUAGE_KEY).as_deref(),
            &manager,
        )
    };
    let scope = conversation_scope(proxy.get_current_dir().ok().as_deref());

    let status = match session::check(ttl, &identity, scope.as_deref()) {
        session::Continuity::Continued {
            id,
            messages,
            age,
            scope,
        } => {
            let left = ttl
                .and_then(|ttl| ttl.checked_sub(age))
                .map(|left| format!(", idle for {}s more", left.as_secs()))
                .unwrap_or_default();
            let root = scope
                .map(|scope| format!(", root {}", scope.display()))
                .unwrap_or_default();
            format!(
                "chat session {id} - {messages} message(s), {}s old{root}{left}; a follow-up `!` continues this",
                age.as_secs()
            )
        }
        session::Continuity::Fresh { stored, reasons } => {
            if ttl.is_none() {
                format!(
                    "no chat session ({} is 0, so `!` turns do not share a conversation)",
                    session::SESSION_TTL_KEY
                )
            } else if stored {
                format!(
                    "no chat session carried (would start fresh: {})",
                    reasons.join("; ")
                )
            } else {
                "no chat session carried".to_string()
            }
        }
    };

    StatusReport {
        status,
        jobs: jobs::describe_running(),
    }
}

/// Note appended to `chat_model` output when a carried conversation exists.
///
/// A model change does not end the conversation - the history is
/// model-agnostic text - so say so while the cause is still obvious, rather
/// than leaving the next `!`'s continuation a surprise. Pure text built from
/// the same age-only description `chat_reset` prints; `chat_status` remains
/// the arbiter for whether identity or scope would still break it.
pub(super) fn carried_model_note(proxy: &mut dyn ShellProxy) -> String {
    session::session_description(resolve_session_ttl(proxy))
        .map(|detail| format!(" (carried conversation continues with the new model: {detail})"))
        .unwrap_or_default()
}

/// Warning for a misconfigured `AI_CHAT_SESSION_TTL_SECS`. `resolve_ttl`
/// silently falls back to the default on unparsable input, so a typo would
/// otherwise never surface. `None` when the setting is absent (default
/// applies) or valid (`0` disables, anything else parses).
pub(super) fn invalid_ttl_notice(proxy: &mut dyn ShellProxy) -> Option<String> {
    let raw = resolve_setting(proxy, session::SESSION_TTL_KEY)?;
    raw.trim().parse::<u64>().err().map(|_| {
        format!(
            "chat: {}={raw:?} is not a number of seconds; using the default 1800s",
            session::SESSION_TTL_KEY
        )
    })
}

/// Describe the carried conversation, for `doctor ai`.
pub fn chat_session_description(proxy: &mut dyn ShellProxy) -> Option<String> {
    session::session_description(resolve_session_ttl(proxy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell_capabilities::AgentCommandPolicy;
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

    /// Seeds the process-wide session slot the way a finished turn leaves
    /// it, with the identity and scope this proxy would compute itself.
    fn seed_session(proxy: &mut TestShellProxy, id: &str) {
        let manager = proxy.agent_mcp_manager();
        let identity = assemble_system_prompt("", None, None, &manager.read());
        let scope = conversation_scope(Some(&proxy.current_dir));
        session::store(
            resolve_session_ttl(proxy),
            ConversationManager::new(
                serde_json::json!({"role": "system", "content": "sys"}),
                serde_json::json!({"role": "user", "content": "goal"}),
            ),
            id,
            &identity,
            scope,
            None,
        );
    }

    #[test]
    fn status_report_continues_a_matching_session() {
        let _guard = session::tests::TEST_LOCK.lock().unwrap();
        let _state = session::tests::isolated_state_home();
        session::session_reset();
        let mut proxy = TestShellProxy::default();
        seed_session(&mut proxy, "s1");

        let report = status_report(&mut proxy);
        assert!(report.status.contains("s1"), "{}", report.status);
        assert!(
            report.status.contains("continues this"),
            "{}",
            report.status
        );
    }

    #[test]
    fn status_report_names_a_prompt_change_instead_of_claiming_continuity() {
        let _guard = session::tests::TEST_LOCK.lock().unwrap();
        let _state = session::tests::isolated_state_home();
        session::session_reset();
        let mut proxy = TestShellProxy::default();
        seed_session(&mut proxy, "s1");
        proxy
            .vars
            .insert(PROMPT_KEY.to_string(), "custom".to_string());

        let report = status_report(&mut proxy);
        assert!(
            report.status.contains("would start fresh"),
            "{}",
            report.status
        );
        assert!(report.status.contains("prompt"), "{}", report.status);
    }

    #[test]
    fn status_report_honors_a_disabled_ttl() {
        let _guard = session::tests::TEST_LOCK.lock().unwrap();
        let _state = session::tests::isolated_state_home();
        session::session_reset();
        let mut proxy = TestShellProxy::default();
        seed_session(&mut proxy, "s1");
        proxy
            .vars
            .insert(session::SESSION_TTL_KEY.to_string(), "0".to_string());

        let report = status_report(&mut proxy);
        assert!(report.status.contains("is 0"), "{}", report.status);
    }

    #[test]
    fn carried_model_note_names_the_session_only_when_one_is_carried() {
        let _guard = session::tests::TEST_LOCK.lock().unwrap();
        let _state = session::tests::isolated_state_home();
        session::session_reset();
        let mut proxy = TestShellProxy::default();
        assert_eq!(carried_model_note(&mut proxy), "");

        seed_session(&mut proxy, "s1");
        let note = carried_model_note(&mut proxy);
        assert!(note.contains("s1"), "{note}");
        assert!(note.contains("continues with the new model"), "{note}");
    }

    #[test]
    fn invalid_ttl_notice_fires_only_on_unparsable_values() {
        let mut proxy = TestShellProxy::default();
        assert_eq!(invalid_ttl_notice(&mut proxy), None);

        proxy
            .vars
            .insert(session::SESSION_TTL_KEY.to_string(), "90".to_string());
        assert_eq!(invalid_ttl_notice(&mut proxy), None);

        proxy
            .vars
            .insert(session::SESSION_TTL_KEY.to_string(), "0".to_string());
        assert_eq!(invalid_ttl_notice(&mut proxy), None);

        proxy.vars.insert(
            session::SESSION_TTL_KEY.to_string(),
            "ten-minutes".to_string(),
        );
        let notice = invalid_ttl_notice(&mut proxy).expect("typo must warn");
        assert!(notice.contains("ten-minutes"), "{notice}");
        assert!(notice.contains("1800s"), "{notice}");
    }
}
