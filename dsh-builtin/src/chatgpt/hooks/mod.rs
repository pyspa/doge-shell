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
use tracing::debug;

use crate::ShellProxy;

pub(crate) mod config;
mod runner;

pub(crate) use config::{HOOKS_ENABLED_KEY, HookEvent, LoadedHooks};

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
        }
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

        Ok(Self {
            hooks,
            session_id: String::new(),
            turn_id: new_id(),
            cwd: proxy.get_current_dir().unwrap_or_default(),
            safety_level: proxy.safety_level().as_str(),
            agent_task_id,
        })
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
        tool: Option<&str>,
        detail: impl FnOnce() -> Value,
        cancel: &dyn Fn() -> bool,
    ) -> HookOutcome {
        if self.hooks.is_empty() {
            return HookOutcome::default();
        }
        let hooks = self.hooks.matching(event, tool);
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
            let env = self.env_for(hook, event, tool);
            match runner::run_hook(hook, &payload, &env, &self.cwd, cancel) {
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
                        let reason =
                            format!("hook failed ({err}). Fix it, or set {HOOKS_ENABLED_KEY}=off.");
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
        let mut payload = json!({
            "hook_version": 1,
            "event": event.as_str(),
            "session_id": self.session_id,
            "turn_id": self.turn_id,
            "cwd": self.cwd.display().to_string(),
            "safety_level": self.safety_level,
            "agent_task_id": self.agent_task_id,
            "timestamp": chrono::Utc::now().to_rfc3339(),
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
    ) -> Vec<(String, String)> {
        // Duplicated with the payload on purpose: a three-line hook should not
        // need a JSON parser to answer "which tool is this?".
        let mut env = vec![
            ("DSH_HOOK_EVENT".to_string(), event.as_str().to_string()),
            ("DSH_HOOK_ID".to_string(), hook.id.clone()),
            ("DSH_HOOK_SESSION_ID".to_string(), self.session_id.clone()),
            ("DSH_HOOK_TURN_ID".to_string(), self.turn_id.clone()),
            ("DSH_HOOK_CWD".to_string(), self.cwd.display().to_string()),
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
mod tests {
    use super::*;

    fn ask(hook: &str) -> HookDecision {
        HookDecision::Ask {
            hook: hook.to_string(),
            reason: "r".to_string(),
        }
    }

    fn deny(hook: &str) -> HookDecision {
        HookDecision::Deny {
            hook: hook.to_string(),
            reason: "r".to_string(),
        }
    }

    #[test]
    fn decisions_merge_to_the_strictest() {
        assert_eq!(
            HookDecision::Continue.merge(ask("a")),
            ask("a"),
            "an ask beats carrying on"
        );
        assert_eq!(ask("a").merge(deny("b")), deny("b"), "a deny beats an ask");
        assert_eq!(
            deny("b").merge(ask("a")),
            deny("b"),
            "order does not matter"
        );
        assert_eq!(
            HookDecision::Continue.merge(HookDecision::Continue),
            HookDecision::Continue
        );
    }

    /// Whatever two hooks say, the merge is at least as strict as each of them.
    #[test]
    fn merge_never_weakens_an_input() {
        let all = [HookDecision::Continue, ask("a"), deny("b")];
        for left in &all {
            for right in &all {
                let merged = left.clone().merge(right.clone());
                assert!(merged >= *left, "{merged:?} < {left:?}");
                assert!(merged >= *right, "{merged:?} < {right:?}");
            }
        }
    }

    /// There is no way to express approval, and that is the point.
    #[test]
    fn the_decision_type_has_no_allow() {
        // A compile-time property; asserted here so deleting it is a test
        // failure rather than a silent widening of what a hook may do.
        match HookDecision::Continue {
            HookDecision::Continue | HookDecision::Ask { .. } | HookDecision::Deny { .. } => {}
        }
    }

    #[test]
    fn a_disabled_context_fires_nothing() {
        let ctx = HookContext::disabled();
        let outcome = ctx.fire(
            HookEvent::PreToolUse,
            Some("execute"),
            || json!({}),
            &never_cancelled,
        );
        assert_eq!(outcome.decision, HookDecision::Continue);
        assert!(outcome.context.is_empty());
    }

    #[test]
    fn the_reentrancy_guard_admits_one_holder() {
        let first = ReentryGuard::acquire();
        assert!(first.is_some());
        assert!(ReentryGuard::acquire().is_none());
        drop(first);
        assert!(ReentryGuard::acquire().is_some());
    }

    #[test]
    fn tool_arguments_are_masked_before_a_hook_sees_them() {
        let detail = tool_detail(
            "execute",
            Some("call_1"),
            "builtin",
            r#"{"command":"AWS_SECRET_ACCESS_KEY=abcd1234 deploy"}"#,
        );
        let raw = detail["tool_arguments_raw"].as_str().unwrap();

        assert!(!raw.contains("abcd1234"), "{raw}");
        assert_eq!(detail["tool_name"], "execute");
        assert_eq!(detail["tool_kind"], "builtin");
    }

    /// Arguments that are not JSON still reach the hook as a string.
    #[test]
    fn unparseable_arguments_leave_the_structured_field_null() {
        let detail = tool_detail("execute", None, "builtin", "not json");
        assert!(detail["tool_arguments"].is_null());
        assert_eq!(detail["tool_arguments_raw"], "not json");
    }

    fn hooks_from(json: &str) -> LoadedHooks {
        config::parse(json).expect("test hook config")
    }

    fn script(dir: &tempfile::TempDir, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join("hook.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    fn context(dir: &tempfile::TempDir, events: &str, body: &str) -> HookContext {
        let command = script(dir, body);
        HookContext::with_hooks(
            hooks_from(&format!(
                r#"{{"version":1,"hooks":[{{"id":"probe","events":{events},"command":["{command}"]}}]}}"#
            )),
            dir.path().to_path_buf(),
        )
    }

    /// A gate that can be got past by crashing is not a gate.
    #[test]
    fn gate_event_fails_closed_on_hook_failure() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir, r#"["pre-tool-use"]"#, "exit 9");

        let outcome = ctx.fire(
            HookEvent::PreToolUse,
            Some("execute"),
            || json!({}),
            &never_cancelled,
        );

        let (hook, reason) = outcome.denied().expect("a broken gate must refuse");
        assert_eq!(hook, "probe");
        assert!(reason.contains("hook failed"), "{reason}");
        assert!(reason.contains(HOOKS_ENABLED_KEY), "{reason}");
    }

    /// An observer that breaks must not take the shell down with it.
    #[test]
    fn observation_event_fails_open_on_hook_failure() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir, r#"["post-tool-use"]"#, "exit 9");

        let outcome = ctx.fire(
            HookEvent::PostToolUse,
            Some("execute"),
            || json!({}),
            &never_cancelled,
        );

        assert_eq!(outcome.decision, HookDecision::Continue);
    }

    /// `ask` on an observation event has nothing left to ask about.
    #[test]
    fn an_ask_on_an_observation_event_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(
            &dir,
            r#"["response-complete"]"#,
            r#"echo '{"decision":"ask","reason":"too late"}'"#,
        );

        let outcome = ctx.fire(
            HookEvent::ResponseComplete,
            None,
            || json!({}),
            &never_cancelled,
        );

        assert_eq!(outcome.decision, HookDecision::Continue);
    }

    #[test]
    fn additional_context_is_collected() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(
            &dir,
            r#"["post-tool-use"]"#,
            r#"echo '{"additional_context":"repo policy: no edits under /etc"}'"#,
        );

        let outcome = ctx.fire(
            HookEvent::PostToolUse,
            Some("edit"),
            || json!({}),
            &never_cancelled,
        );

        assert_eq!(
            outcome.context_note().as_deref(),
            Some("repo policy: no edits under /etc")
        );
    }

    /// The process-environment flag, not a shell variable: a shell variable
    /// could be cleared from inside the hook, which is the loop this prevents.
    #[test]
    fn nested_depth_disables_every_hook() {
        let _lock = crate::chatgpt::tool::execute::tests::env_lock();
        // SAFETY: single-threaded under `env_lock`.
        unsafe { std::env::set_var(config::HOOK_DEPTH_ENV, "1") };
        let nested = config::nested_in_a_hook();
        let mut proxy = crate::test_support::TestShellProxy::default();
        let loaded = config::load(&mut proxy as &mut dyn ShellProxy);
        unsafe { std::env::remove_var(config::HOOK_DEPTH_ENV) };

        assert!(nested);
        assert!(loaded.expect("nesting is not an error").is_empty());
        assert!(!config::nested_in_a_hook());
    }

    #[test]
    fn the_off_switch_stops_the_file_from_being_read() {
        let _lock = crate::chatgpt::tool::execute::tests::env_lock();
        let mut proxy = crate::test_support::TestShellProxy::default();
        proxy
            .vars
            .insert(HOOKS_ENABLED_KEY.to_string(), "off".to_string());
        proxy.vars.insert(
            config::HOOKS_CONFIG_KEY.to_string(),
            "/definitely/not/here.json".to_string(),
        );

        let loaded = config::load(&mut proxy as &mut dyn ShellProxy);

        assert!(loaded.expect("off means off, not an error").is_empty());
    }

    /// An override pointing at nothing is a typo, and a typo that silently
    /// disables every check is the failure this whole module guards against.
    #[test]
    fn an_override_that_points_at_nothing_is_an_error() {
        let _lock = crate::chatgpt::tool::execute::tests::env_lock();
        let mut proxy = crate::test_support::TestShellProxy::default();
        proxy.vars.insert(
            config::HOOKS_CONFIG_KEY.to_string(),
            "/definitely/not/here.json".to_string(),
        );

        let err = config::load(&mut proxy as &mut dyn ShellProxy)
            .expect_err("a broken override must be reported");
        assert!(err.contains("not a file"), "{err}");
    }

    #[test]
    fn the_payload_carries_the_common_envelope() {
        let mut ctx = HookContext::disabled();
        ctx.set_session_id("sess".to_string());

        let payload = ctx.payload(HookEvent::UserPromptSubmit, json!({"prompt": "hi"}));

        assert_eq!(payload["hook_version"], 1);
        assert_eq!(payload["event"], "user-prompt-submit");
        assert_eq!(payload["session_id"], "sess");
        assert_eq!(payload["prompt"], "hi");
        assert!(payload["timestamp"].as_str().is_some());
    }
}
