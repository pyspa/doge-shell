//! Environment-variable keys and the `resolve_*` functions that read them
//! (shell variable first, then process environment - see `resolve_setting`).
//! One place per setting so `chat_reset`/`chat_status`/`chat_with_tools`
//! cannot drift about what a key means or defaults to.
use super::*;

/// Environment variable key for storing the chat prompt template
pub(super) const PROMPT_KEY: &str = "CHAT_PROMPT";
/// Primary configuration key for storing the default model
pub(super) const MODEL_KEY: &str = "AI_CHAT_MODEL";
/// Environment variable key for storing the AI response language
pub(super) const LANGUAGE_KEY: &str = "AI_MESSAGE_LANG";
/// Maximum number of iterations to satisfy tool calls before aborting.
/// Shared with the shell-side loop so the two cannot drift apart.
pub(super) use dsh_openai::turn::limits::MAX_TOOL_ITERATIONS;
/// Threshold of characters in the buffer to trigger summarization (~3k-12k tokens)
pub(super) const MAX_BUFFER_CHARS: usize = 96000;
/// Environment variable key to override the model used for summarization
pub(super) const SUMMARY_MODEL_KEY: &str = "AI_SUMMARY_MODEL";
/// Environment key overriding the prompt-token ceiling before summarizing.
pub(super) const CONTEXT_TOKEN_BUDGET_KEY: &str = "AI_CHAT_CONTEXT_TOKEN_BUDGET";
/// Default prompt-token ceiling before the conversation is summarized.
pub(super) const DEFAULT_CONTEXT_TOKEN_BUDGET: u64 = 100_000;
/// Environment key capping what one `!` turn may spend, in total tokens.
pub(super) const TURN_TOKEN_BUDGET_KEY: &str = "AI_CHAT_TURN_TOKEN_BUDGET";
/// Summarization attempts allowed before one turn gives up and proceeds.
pub(super) const MAX_SUMMARY_ROUNDS: usize = 3;
/// Per-tool-result budget inside the text handed to the summarizer.
pub(super) const MAX_SUMMARY_TOOL_CHARS: usize = 500;
/// Buffer *messages* kept verbatim by the deterministic compaction pass.
///
/// Counted in messages because that is what `retain_boundary` takes, and a
/// tool result always follows the assistant message that asked for it - so
/// this preserves roughly half as many results as its value suggests.
pub(super) const RECENT_BUFFER_MESSAGES_KEPT: usize = 8;
/// A tool result this small is not worth a stub: the replacement text costs
/// about as much as the result did.
pub(super) const MIN_ELIDABLE_TOOL_CHARS: usize = 400;
/// Cache-routing hint for providers that support it.
pub(super) const PROMPT_CACHE_KEY: &str = "dsh-chat-agent";
/// Environment key for how long an interactive `execute` waits before handing
/// the model a job handle instead of a result.
pub(crate) const EXECUTE_YIELD_MS_KEY: &str = "AI_CHAT_EXECUTE_YIELD_MS";
/// Long enough that the ordinary command still answers in one round trip,
/// short enough that a build does not hold the shell.
const DEFAULT_EXECUTE_YIELD_MS: u64 = 10_000;
/// The schema advertises this as `yield_time_ms`'s maximum, and both the
/// environment default and the model-supplied argument are clamped to it -
/// providers violate a schema routinely, and an unclamped value would hold the
/// shell for as long as the command runs.
pub(crate) const MAX_EXECUTE_YIELD_MS: u64 = 60_000;
/// Environment key overriding the interactive `execute` timeout.
pub(crate) const EXECUTE_TIMEOUT_MS_KEY: &str = "AI_CHAT_EXECUTE_TIMEOUT_MS";
/// Environment key toggling incremental Markdown rendering for `!` chat.
///
/// Streaming is opt-out, not opt-in: the escape hatch exists for a server
/// whose SSE support is broken in a way `send_chat_streaming`'s own
/// fallbacks do not catch, and for anyone who prefers the old
/// print-once-at-the-end behavior.
pub(super) const STREAM_KEY: &str = "AI_CHAT_STREAM";
/// Environment key turning off skills carried by the current repository.
///
/// A `.dogesh/skills` directory arrives with a `git clone`, so its summaries reach
/// the model the first time `!` is used in that checkout. Personal skills stay
/// available when this is off.
pub(super) const PROJECT_SKILLS_KEY: &str = "AI_CHAT_PROJECT_SKILLS";
/// Environment key controlling whether `skill_manage` writes are staged for
/// review instead of landing immediately. `task` (default) / `always` / `off`.
pub(super) const SKILL_STAGING_KEY: &str = "AI_CHAT_SKILL_STAGING";
/// Environment key turning on the turn-end skill reviewer. Off by default.
pub(super) const SKILL_REFLECT_KEY: &str = "AI_CHAT_SKILL_REFLECT";
/// Environment key for the reviewer's tool-call threshold. Default 5.
pub(super) const SKILL_REFLECT_MIN_TOOLS_KEY: &str = "AI_CHAT_SKILL_REFLECT_MIN_TOOLS";
/// Environment key overriding the model the reviewer uses. Defaults to
/// `AI_SUMMARY_MODEL`, then the turn's own model.
pub(super) const SKILL_REFLECT_MODEL_KEY: &str = "AI_CHAT_SKILL_REFLECT_MODEL";
/// Environment key enabling the optional archive sweep. `0` (default) means
/// off; a positive integer is the number of unread days before an
/// agent-written, unpinned, `user`-scope skill is archived.
pub(super) const SKILL_AUTO_ARCHIVE_DAYS_KEY: &str = "AI_CHAT_SKILL_AUTO_ARCHIVE_DAYS";
/// Environment key turning on a one-shot verification nudge for `!` chat.
/// Off by default: when on, a turn that ran a mutating tool (`edit` /
/// `str_replace` / `execute` / `skill_manage` / `mcp__*`) gets its first
/// final answer bounced back once with a request to state what was checked.
/// The second answer is always accepted, so this costs at most one extra
/// round trip per mutating turn.
pub(super) const VERIFY_AFTER_MUTATION_KEY: &str = "AI_CHAT_VERIFY_AFTER_MUTATION";
/// Told to the model after a rewound turn (`ConversationManager::note_turn_rewound`).
pub(super) const REWIND_NOTICE: &str = "The previous turn was removed from this conversation because it did not finish. Any tool calls it made may already have taken effect; check the actual state rather than assuming.";

/// Read a setting shell-variable first, then the process environment.
///
/// `proxy.set_var` (and `(vset ...)`) writes into the shell `Environment`, not
/// the process env, so an env-only lookup silently ignores it.
/// How long an interactive `execute` waits for the command before yielding.
///
/// `0` is allowed and means "always hand back a handle", which is what an
/// agent task effectively does with its 1s ceiling.
pub(crate) fn resolve_execute_yield_ms(proxy: &mut dyn ShellProxy) -> u64 {
    resolve_setting(proxy, EXECUTE_YIELD_MS_KEY)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_EXECUTE_YIELD_MS)
        .min(MAX_EXECUTE_YIELD_MS)
}

/// The default `timeout_ms` for an interactive `execute`.
pub(crate) fn resolve_execute_timeout_ms(proxy: &mut dyn ShellProxy, default: u64) -> u64 {
    resolve_setting(proxy, EXECUTE_TIMEOUT_MS_KEY)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

pub(super) fn resolve_setting(proxy: &mut dyn ShellProxy, key: &str) -> Option<String> {
    proxy
        .get_var(key)
        .or_else(|| std::env::var(key).ok())
        .filter(|value| !value.trim().is_empty())
}

/// The one place `AI_CHAT_SESSION_TTL_SECS` is read, so `chat_reset`,
/// `chat_status`, `chat_session_description` and `chat_with_tools` cannot
/// drift from each other about how long a conversation is carried forward.
pub(super) fn resolve_session_ttl(proxy: &mut dyn ShellProxy) -> Option<Duration> {
    session::resolve_ttl(resolve_setting(proxy, session::SESSION_TTL_KEY))
}

/// The project boundary a turn's conversation continuity is scoped to.
///
/// A thin, named wrapper around `tool::workspace_root` so the wiring between
/// a turn's cwd and the scope handed to `session::take`/`store` has a focused
/// unit test of its own, cheaper than driving the whole turn through
/// `chat_with_tools` (see `tests::ScriptedClient` for that route).
pub(super) fn conversation_scope(cwd: Option<&Path>) -> Option<PathBuf> {
    cwd.map(tool::workspace_root)
}

/// Whether `!` chat should stream its answer as it is generated.
///
/// Default on: `0` / `false` / `off` / `no` (case-insensitive) opt out.
pub(super) fn resolve_stream_enabled(proxy: &mut dyn ShellProxy) -> bool {
    match resolve_setting(proxy, STREAM_KEY) {
        None => true,
        Some(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
    }
}

/// Ceiling on what one turn may spend, or `None` when unset.
///
/// `MAX_TOOL_ITERATIONS` bounds the number of steps, not their cost: a hundred
/// iterations over a large context is a bill, not a guard rail. Off by default,
/// because the right number depends on the model and on what the user is
/// willing to spend.
pub(super) fn resolve_turn_token_budget(proxy: &mut dyn ShellProxy) -> Option<u64> {
    resolve_setting(proxy, TURN_TOKEN_BUDGET_KEY)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|budget| *budget > 0)
}

/// Prompt-token ceiling that forces a summary regardless of buffer bytes.
/// Where the loop is, in the shape the hook layer publishes.
///
/// One place so that the three call sites in `chat_with_tools` cannot drift
/// apart and hand a hook two different views of the same round.
pub(super) fn loop_state(
    iterations: usize,
    prompt_tokens: u64,
    completion_tokens: u64,
    turn_token_budget: Option<u64>,
) -> hooks::LoopState {
    hooks::LoopState {
        iteration: u32::try_from(iterations).unwrap_or(u32::MAX),
        max_iterations: u32::try_from(MAX_TOOL_ITERATIONS).unwrap_or(u32::MAX),
        prompt_tokens,
        completion_tokens,
        turn_token_budget,
    }
}

pub(super) fn resolve_prompt_token_budget(proxy: &mut dyn ShellProxy) -> u64 {
    resolve_setting(proxy, CONTEXT_TOKEN_BUDGET_KEY)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|budget| *budget > 0)
        .unwrap_or(DEFAULT_CONTEXT_TOKEN_BUDGET)
}

/// Whether skills carried by the current repository may reach the prompt.
///
/// On by default. A cloned repository can put text in front of the model just
/// by existing, so there has to be a way to turn that off without also giving
/// up personal skills.
pub(crate) fn resolve_project_skills_enabled(proxy: &mut dyn ShellProxy) -> bool {
    match resolve_setting(proxy, PROJECT_SKILLS_KEY) {
        None => true,
        Some(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
    }
}

/// Whether `skill_manage` writes land on disk immediately or wait for a
/// person to `skill approve` them.
///
/// - `task` (default): only staged when there is an agent task *and* that
///   task's `--write` grant does not already cover the target. A task that
///   was given the grant writes exactly as it does today; only the path that
///   used to stall on `InputRequired` now stages instead. An interactive `!`
///   session is unaffected either way.
/// - `always`: every write is staged, interactive or not - for a person who
///   wants to review every skill change before it lands.
/// - `off`: today's behaviour. A task with no grant still stalls on
///   `InputRequired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillStaging {
    Off,
    Task,
    Always,
}

pub(crate) fn resolve_skill_staging(proxy: &mut dyn ShellProxy) -> SkillStaging {
    match resolve_setting(proxy, SKILL_STAGING_KEY)
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("task") => SkillStaging::Task,
        Some("always") => SkillStaging::Always,
        Some("off") => SkillStaging::Off,
        Some(_) => SkillStaging::Task,
    }
}

/// Whether a mutating `!` turn bounces its first final answer back once for
/// verification. Off by default: the extra round trip costs latency and
/// tokens, so only operators who want the guard pay it.
pub(super) fn resolve_verify_after_mutation(proxy: &mut dyn ShellProxy) -> bool {
    match resolve_setting(proxy, VERIFY_AFTER_MUTATION_KEY) {
        None => false,
        Some(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
    }
}

/// The operator's response language, for any AI request the shell makes.
///
/// Public because `ai-commit`, `safe-run` and `blocks` need the same answer:
/// `AI_MESSAGE_LANG` used to reach the `!` runtime and nothing else.
pub fn response_language(proxy: &mut dyn ShellProxy) -> Option<String> {
    proxy
        .get_var(LANGUAGE_KEY)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
