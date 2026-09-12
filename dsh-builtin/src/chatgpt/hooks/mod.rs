//! AI chat hooks: the user's own checks, run around the `!` agent loop.
//!
//! Distinct from the Lisp hooks in `dsh/src/shell/hooks.rs`, which fire around
//! ordinary command execution and never see the agent at all.
//!
//! # A hook cannot grant permission
//!
//! `HookDecision` has no `Allow`. `ai-architecture.md` §4 says the safety gate
//! is `SafetyGuard` and nothing else, and the cheapest way to keep that true is
//! to give the hook layer no vocabulary for permitting anything. A hook can:
//!
//! - stop something that was going to happen (`deny`),
//! - insist a person is asked about it (`ask`),
//! - add text the model will read, or a line the user will see,
//! - or say nothing, which means "carry on exactly as before".
//!
//! `Continue` is not approval. Everything downstream - `execute::authorize`,
//! `authorize_mcp_tool`, `confirm_agent_action` - runs unchanged after it.
//!
//! Hooks run before those gates so that a denial costs the user no question.

use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tracing::debug;

use crate::ShellProxy;

pub(crate) mod config;
mod runner;

pub(crate) use config::{
    HOOK_TURN_BUDGET_KEY, HOOKS_ENABLED_KEY, HookEvent, HookSubject, LoadedHooks,
};

/// Where the agent loop is, for a hook that governs the loop rather than one
/// call.
///
/// A `pre-tool-use` hook could previously see the tool and its arguments but
/// not that it was the fortieth iteration of a run that had already spent
/// 200k tokens, which is exactly the shape of a runaway. All zeroes means
/// "outside the tool loop" (`user-prompt-submit`, `session-start`).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LoopState {
    pub iteration: u32,
    pub max_iterations: u32,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub turn_token_budget: Option<u64>,
}

impl LoopState {
    fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// The strongest thing a set of hooks said.
///
/// Ordered so that merging is `max`: `Deny` outranks `Ask` outranks
/// `Continue`. Two hooks disagreeing can only ever end stricter.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum HookDecision {
    #[default]
    Continue,
    Ask {
        hook: String,
        reason: String,
    },
    Deny {
        hook: String,
        reason: String,
    },
}

impl HookDecision {
    fn merge(self, other: HookDecision) -> HookDecision {
        self.max(other)
    }
}

#[derive(Debug, Default)]
pub(crate) struct HookOutcome {
    pub decision: HookDecision,
    /// Text the model should see alongside whatever it was going to get.
    pub context: Vec<String>,
}

impl HookOutcome {
    pub(crate) fn denied(&self) -> Option<(&str, &str)> {
        match &self.decision {
            HookDecision::Deny { hook, reason } => Some((hook.as_str(), reason.as_str())),
            _ => None,
        }
    }

    pub(crate) fn asked(&self) -> Option<(&str, &str)> {
        match &self.decision {
            HookDecision::Ask { hook, reason } => Some((hook.as_str(), reason.as_str())),
            _ => None,
        }
    }

    /// The extra text, joined, or nothing when no hook added any.
    pub(crate) fn context_note(&self) -> Option<String> {
        (!self.context.is_empty()).then(|| self.context.join("\n"))
    }
}

/// One turn's worth of hook state.
///
/// Deliberately holds no `proxy`: the dispatcher must have no way to reach
/// `remember_agent_approval` or the session allowlist, so "a hook cannot grant
/// permission" is a property of the types rather than of this file's discipline.
pub(crate) struct HookContext {
    hooks: LoadedHooks,
    session_id: String,
    turn_id: String,
    cwd: PathBuf,
    safety_level: &'static str,
    agent_task_id: Option<String>,
    /// `Cell` rather than a `&mut self` setter: `fire` is called through a
    /// shared reference from `execute_tool_call`, and the turn budget below has
    /// to be *written* from there too. Single-threaded for the same reason the
    /// re-entrancy guard is thread-local - one turn runs on one thread. Reach
    /// for an atomic if that ever stops being true.
    loop_state: Cell<LoopState>,
    /// `None` is unlimited, like `AI_CHAT_TURN_TOKEN_BUDGET`.
    turn_budget_ms: Option<u64>,
    spent_ms: Cell<u64>,
    /// Said once per turn, not once per process: `warn_once` dedupes for the
    /// life of the shell and shares its key space with hook *failures*, so
    /// using it here would announce the first skip only, and would then
    /// suppress the report of a later real crash of that same hook.
    budget_warned: Cell<bool>,
}

impl HookContext {
    /// A context that never fires anything.
    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self {
            hooks: LoadedHooks::default(),
            session_id: String::new(),
            turn_id: String::new(),
            cwd: PathBuf::new(),
            safety_level: "normal",
            agent_task_id: None,
            loop_state: Cell::default(),
            turn_budget_ms: None,
            spent_ms: Cell::default(),
            budget_warned: Cell::default(),
        }
    }

    /// A context wired to hooks a test built by hand, skipping the file.
    #[cfg(test)]
    pub(crate) fn with_hooks(hooks: LoadedHooks, cwd: PathBuf) -> Self {
        Self {
            hooks,
            session_id: "test-session".to_string(),
            turn_id: "test-turn".to_string(),
            cwd,
            safety_level: "normal",
            agent_task_id: None,
            loop_state: Cell::default(),
            turn_budget_ms: None,
            spent_ms: Cell::default(),
            budget_warned: Cell::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_turn_budget(mut self, budget_ms: u64) -> Self {
        self.turn_budget_ms = Some(budget_ms);
        self
    }

    /// Read the configuration for this turn.
    ///
    /// An unreadable configuration is an error, not an empty set: a list of
    /// checks that silently stopped running is the worst outcome available.
    pub(crate) fn load(
        proxy: &mut dyn crate::shell_capabilities::ChatToolHost,
    ) -> Result<Self, String> {
        let hooks = config::load(proxy as &mut dyn ShellProxy)?;
        let agent_task_id = proxy
            .agent_runtime()
            .map(|runtime| runtime.lock().task.id.clone());

        // Only when there is something to budget. `config::load` has already
        // returned empty for `AI_CHAT_HOOKS=off` and for a nested `dsh`, and a
        // malformed budget must not refuse the chat in either case - the
        // documented way out of a broken hook setup is that switch.
        let turn_budget_ms = if hooks.is_empty() {
            None
        } else {
            config::turn_budget_ms(proxy as &mut dyn ShellProxy)?
        };

        Ok(Self {
            hooks,
            session_id: String::new(),
            turn_id: new_id(),
            cwd: proxy.get_current_dir().unwrap_or_default(),
            safety_level: proxy.safety_level().as_str(),
            agent_task_id,
            loop_state: Cell::default(),
            turn_budget_ms,
            spent_ms: Cell::default(),
            budget_warned: Cell::default(),
        })
    }

    /// Tell the hook layer where the loop is. Cheap enough to call every round.
    pub(crate) fn note_loop(&self, state: LoopState) {
        self.loop_state.set(state);
    }

    /// How long this hook may run, or `None` when the turn budget is gone and
    /// the event is one that can be skipped.
    ///
    /// **A gate is never skipped.** A check that can be got past by being slow
    /// is not a check, so an exhausted budget shortens a gate's timeout (down
    /// to `MIN_TIMEOUT_MS`) instead, and a gate that then times out is a
    /// `HookRun::Failed`, which for a gate already means deny. Slowness cannot
    /// buy permission. Observation events carry no decision, so cutting them is
    /// a cost the user chose when they set the budget.
    fn time_slice(&self, hook: &config::HookDefinition, event: HookEvent) -> Option<Duration> {
        let configured = Duration::from_millis(hook.timeout_ms());
        let Some(budget) = self.turn_budget_ms else {
            return Some(configured);
        };
        let remaining = budget.saturating_sub(self.spent_ms.get());
        if remaining >= hook.timeout_ms() {
            return Some(configured);
        }
        if remaining >= config::MIN_TIMEOUT_MS {
            return Some(Duration::from_millis(remaining));
        }
        if event.is_gate() {
            return Some(Duration::from_millis(config::MIN_TIMEOUT_MS));
        }
        None
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn set_session_id(&mut self, id: String) {
        self.session_id = id;
    }

    pub(crate) fn new_session_id(&self) -> String {
        self.agent_task_id.clone().unwrap_or_else(new_id)
    }

    /// Run every hook registered for `event`.
    ///
    /// `detail` is a closure because building it is not free - the tool payload
    /// masks the whole tool result - and the overwhelmingly common case is a
    /// shell with no hooks at all, where none of it is ever read.
    pub(crate) fn fire(
        &self,
        event: HookEvent,
        subject: HookSubject<'_>,
        detail: impl FnOnce() -> Value,
        cancel: &dyn Fn() -> bool,
    ) -> HookOutcome {
        // Ahead of building `MatchInput` on purpose: the overwhelmingly common
        // shell has no hooks at all, and it must not pay to find that out.
        if self.hooks.is_empty() {
            return HookOutcome::default();
        }
        let input = config::MatchInput::new(subject, &self.cwd);
        let hooks = self.hooks.matching(event, &input);
        if hooks.is_empty() {
            return HookOutcome::default();
        }

        // Belt and braces alongside `DSH_HOOK_DEPTH`: that flag stops a hook's
        // child shell, this stops a re-entry inside this process.
        let Some(_guard) = ReentryGuard::acquire() else {
            debug!("skipping {} hooks: already inside a hook", event.as_str());
            return HookOutcome::default();
        };

        let payload = self.payload(event, detail());
        let payload = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
        let mut outcome = HookOutcome::default();

        for hook in hooks {
            let Some(timeout) = self.time_slice(hook, event) else {
                if !self.budget_warned.replace(true) {
                    eprintln!(
                        "\x1b[2mhooks: skipping the observation hooks left in this turn; \
                         it has used its {HOOK_TURN_BUDGET_KEY} of {}ms\x1b[0m",
                        self.turn_budget_ms.unwrap_or_default()
                    );
                }
                debug!(
                    "skipping hook {} on {}: turn hook budget exhausted",
                    hook.id,
                    event.as_str()
                );
                continue;
            };
            let shortened = timeout < Duration::from_millis(hook.timeout_ms());
            let env = self.env_for(hook, event, subject.tool, timeout);
            let started = std::time::Instant::now();
            let run = runner::run_hook(hook, &payload, &env, &self.cwd, timeout, cancel);
            // Only the hook's own time. The approval prompt a gate may raise
            // lives in the caller, so a person thinking about a question cannot
            // spend the budget that decides the next gate.
            self.spent_ms.set(
                self.spent_ms
                    .get()
                    .saturating_add(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
            );
            match run {
                runner::HookRun::Answered(response) => {
                    if let Some(message) = response.message.as_deref().map(str::trim)
                        && !message.is_empty()
                    {
                        // Never silent: a gate the user cannot see is a gate
                        // they will blame on the model.
                        eprintln!("\x1b[2mhook {}: {message}\x1b[0m", hook.id);
                    }
                    if let Some(extra) = response.additional_context.as_deref().map(str::trim)
                        && !extra.is_empty()
                    {
                        if event.uses_context() {
                            outcome.context.push(extra.to_string());
                        } else {
                            // Say so rather than dropping it: the hook author
                            // believes this text reached the model.
                            warn_once(
                                &hook.id,
                                event,
                                "returned additional_context, which this event has nowhere to put",
                            );
                        }
                    }

                    let reason = response
                        .reason
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or("no reason given")
                        .to_string();

                    let decision = match response.decision.as_deref() {
                        Some("deny") if event.is_gate() => HookDecision::Deny {
                            hook: hook.id.clone(),
                            reason,
                        },
                        // `post-tool-use` cannot un-run the tool, but it can
                        // stop the model from treating the result as a success.
                        Some("deny") if event == HookEvent::PostToolUse => HookDecision::Deny {
                            hook: hook.id.clone(),
                            reason,
                        },
                        Some("ask") if event.is_gate() => HookDecision::Ask {
                            hook: hook.id.clone(),
                            reason,
                        },
                        Some(other) => {
                            debug!(
                                "hook {} answered `{other}` on {}, which that event ignores",
                                hook.id,
                                event.as_str()
                            );
                            HookDecision::Continue
                        }
                        None => HookDecision::Continue,
                    };

                    if decision != HookDecision::Continue {
                        announce(&hook.id, event, &decision);
                    }
                    outcome.decision = std::mem::take(&mut outcome.decision).merge(decision);
                }
                runner::HookRun::Failed(err) => {
                    if event.is_gate() {
                        // A gate that can be got past by being slow, or by
                        // crashing, is not a gate.
                        let budget_note = if shortened {
                            format!(
                                " Its timeout was shortened to {}ms by {HOOK_TURN_BUDGET_KEY}.",
                                timeout.as_millis()
                            )
                        } else {
                            String::new()
                        };
                        let reason = format!(
                            "hook failed ({err}).{budget_note} \
                             Fix it, or set {HOOKS_ENABLED_KEY}=off."
                        );
                        let decision = HookDecision::Deny {
                            hook: hook.id.clone(),
                            reason,
                        };
                        announce(&hook.id, event, &decision);
                        outcome.decision = std::mem::take(&mut outcome.decision).merge(decision);
                    } else {
                        warn_once(&hook.id, event, &err);
                    }
                }
            }
        }

        outcome
    }

    fn payload(&self, event: HookEvent, detail: Value) -> Value {
        let state = self.loop_state.get();
        // `hook_version` stays 1: fields are only ever added here, so a hook
        // reading the keys it knows is unaffected. Bump it only to change what
        // an existing key means, or to remove one.
        let mut payload = json!({
            "hook_version": 1,
            "event": event.as_str(),
            "session_id": self.session_id,
            "turn_id": self.turn_id,
            "cwd": self.cwd.display().to_string(),
            "safety_level": self.safety_level,
            "agent_task_id": self.agent_task_id,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            // Nested rather than flattened: `response-complete` already carries
            // an `iterations` total in its own detail, and two keys a letter
            // apart meaning different things is a trap for hook authors.
            "loop": {
                "iteration": state.iteration,
                "max_iterations": state.max_iterations,
                "tokens": {
                    "prompt": state.prompt_tokens,
                    "completion": state.completion_tokens,
                    "total": state.total_tokens(),
                },
                "turn_token_budget": state.turn_token_budget,
            },
            "hook_budget": {
                "turn_budget_ms": self.turn_budget_ms,
                "spent_ms": self.spent_ms.get(),
            },
        });

        if let (Value::Object(target), Value::Object(extra)) = (&mut payload, detail) {
            for (key, value) in extra {
                target.insert(key, value);
            }
        }

        payload
    }

    fn env_for(
        &self,
        hook: &config::HookDefinition,
        event: HookEvent,
        tool: Option<&str>,
        timeout: Duration,
    ) -> Vec<(String, String)> {
        // Duplicated with the payload on purpose: a three-line hook should not
        // need a JSON parser to answer "which tool is this?".
        let state = self.loop_state.get();
        let mut env = vec![
            ("DSH_HOOK_EVENT".to_string(), event.as_str().to_string()),
            ("DSH_HOOK_ID".to_string(), hook.id.clone()),
            ("DSH_HOOK_SESSION_ID".to_string(), self.session_id.clone()),
            ("DSH_HOOK_TURN_ID".to_string(), self.turn_id.clone()),
            ("DSH_HOOK_CWD".to_string(), self.cwd.display().to_string()),
            (
                "DSH_HOOK_ITERATION".to_string(),
                state.iteration.to_string(),
            ),
            (
                "DSH_HOOK_MAX_ITERATIONS".to_string(),
                state.max_iterations.to_string(),
            ),
            (
                "DSH_HOOK_TURN_TOKENS".to_string(),
                state.total_tokens().to_string(),
            ),
            // The effective value, which the turn budget may have shortened.
            (
                "DSH_HOOK_TIMEOUT_MS".to_string(),
                timeout.as_millis().to_string(),
            ),
        ];
        if let Some(tool) = tool {
            env.push(("DSH_HOOK_TOOL".to_string(), tool.to_string()));
        }
        env
    }
}

/// The payload for a tool event, with secrets masked.
///
/// Masking changes the bytes, so this is for watching and refusing - never for
/// reconstructing the command. That is acceptable here precisely because a hook
/// cannot approve: a mismatch can only cost a false refusal or a missed catch,
/// never a false permit.
pub(crate) fn tool_detail(
    tool_name: &str,
    tool_call_id: Option<&str>,
    kind: &str,
    arguments: &str,
) -> Value {
    let redacted = crate::chatgpt::tool::redact_tool_arguments(arguments);
    json!({
        "tool_name": tool_name,
        "tool_call_id": tool_call_id,
        "tool_kind": kind,
        "tool_arguments": serde_json::from_str::<Value>(&redacted).ok(),
        "tool_arguments_raw": redacted,
    })
}

/// What "always" remembers for a hook's question.
///
/// The `hook:` prefix keeps it out of the boxes the other gates use: an
/// "always" on `execute` must not answer a hook's question, and answering a
/// hook must not authorise the command itself. `agent run --allow-command` and
/// `--allow-mcp` cannot satisfy one of these either.
pub(crate) fn approval_key(hook: &str, subject: &str) -> String {
    format!("hook:{hook}:{subject}")
}

pub(crate) fn redact(text: &str) -> String {
    crate::safety_policy::redact_sensitive_text(text)
}

fn announce(hook: &str, event: HookEvent, decision: &HookDecision) {
    let (verb, reason) = match decision {
        HookDecision::Deny { reason, .. } => ("denied", reason.as_str()),
        HookDecision::Ask { reason, .. } => ("flagged", reason.as_str()),
        HookDecision::Continue => return,
    };
    eprintln!(
        "\x1b[2mhook {hook}: {verb} {} ({reason})\x1b[0m",
        event.as_str()
    );
}

/// Report a broken observer hook once per process, not once per tool call.
fn warn_once(hook: &str, event: HookEvent, err: &str) {
    static WARNED: LazyLock<Mutex<BTreeSet<String>>> =
        LazyLock::new(|| Mutex::new(BTreeSet::new()));

    let key = format!("{hook}:{}", event.as_str());
    let first = WARNED
        .lock()
        .map(|mut warned| warned.insert(key))
        .unwrap_or(false);
    if first {
        eprintln!(
            "\x1b[2mhook {hook}: {} {err} (this event continues; later failures are not repeated)\x1b[0m",
            event.as_str()
        );
    }
    debug!("hook {hook} failed on {}: {err}", event.as_str());
}

thread_local! {
    /// Per thread, not per process.
    ///
    /// The re-entry being prevented is "dispatching hooks from inside a hook
    /// dispatch", which can only happen on the call stack that is already in
    /// one. A process-wide flag would additionally make two concurrent agent
    /// loops silently skip each other's hooks - the failure mode this guard
    /// exists to avoid. The cross-process case is `DSH_HOOK_DEPTH`.
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

struct ReentryGuard;

impl ReentryGuard {
    fn acquire() -> Option<Self> {
        IN_HOOK.with(|flag| {
            if flag.get() {
                return None;
            }
            flag.set(true);
            Some(ReentryGuard)
        })
    }
}

impl Drop for ReentryGuard {
    fn drop(&mut self) {
        IN_HOOK.with(|flag| flag.set(false));
    }
}

/// Passed where there is nothing to cancel against, so the parameter is never
/// silently defaulted at a call site that should have wired one up.
pub(crate) fn never_cancelled() -> bool {
    false
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests;
