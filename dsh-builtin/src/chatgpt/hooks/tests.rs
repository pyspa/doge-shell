use super::*;
use std::cell::Cell;
use std::time::Duration;

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
        HookSubject::tool("execute", "{}"),
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
        HookSubject::tool("execute", "{}"),
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
        HookSubject::tool("execute", "{}"),
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
        HookSubject::none(),
        || json!({}),
        &never_cancelled,
    );

    assert_eq!(outcome.decision, HookDecision::Continue);
}

fn timed_context(
    dir: &tempfile::TempDir,
    events: &str,
    timeout_ms: u64,
    body: &str,
) -> HookContext {
    let command = script(dir, body);
    HookContext::with_hooks(
        hooks_from(&format!(
            r#"{{"version":1,"hooks":[{{"id":"probe","events":{events},"timeout_ms":{timeout_ms},"command":["{command}"]}}]}}"#
        )),
        dir.path().to_path_buf(),
    )
}

#[test]
fn the_payload_carries_the_loop_state() {
    let ctx = HookContext::disabled();
    ctx.note_loop(LoopState {
        iteration: 3,
        max_iterations: 100,
        prompt_tokens: 900,
        completion_tokens: 100,
        turn_token_budget: Some(50_000),
    });

    let payload = ctx.payload(HookEvent::PreToolUse, json!({}));
    assert_eq!(payload["loop"]["iteration"], 3);
    assert_eq!(payload["loop"]["max_iterations"], 100);
    assert_eq!(payload["loop"]["tokens"]["total"], 1000);
    assert_eq!(payload["loop"]["turn_token_budget"], 50_000);
    // Only ever added to, so `hook_version` does not move.
    assert_eq!(payload["hook_version"], 1);
}

#[test]
fn the_payload_carries_the_hook_budget() {
    let ctx = HookContext::disabled().with_turn_budget(5_000);
    let payload = ctx.payload(HookEvent::PostToolUse, json!({}));
    assert_eq!(payload["hook_budget"]["turn_budget_ms"], 5_000);
    assert_eq!(payload["hook_budget"]["spent_ms"], 0);

    let unlimited = HookContext::disabled();
    assert!(
        unlimited.payload(HookEvent::PostToolUse, json!({}))["hook_budget"]["turn_budget_ms"]
            .is_null()
    );
}

/// A three-line hook must not need a JSON parser to see the loop.
#[test]
fn the_loop_state_reaches_a_hook_as_an_env_var() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("seen");
    let ctx = context(
        &dir,
        r#"["post-tool-use"]"#,
        &format!(
            "printf '%s/%s %s' \"$DOGESH_HOOK_ITERATION\" \"$DOGESH_HOOK_MAX_ITERATIONS\" \"$DOGESH_HOOK_TURN_TOKENS\" > {}\n",
            marker.display()
        ),
    );
    ctx.note_loop(LoopState {
        iteration: 7,
        max_iterations: 100,
        prompt_tokens: 20,
        completion_tokens: 5,
        turn_token_budget: None,
    });

    ctx.fire(
        HookEvent::PostToolUse,
        HookSubject::tool("execute", "{}"),
        || json!({}),
        &never_cancelled,
    );

    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "7/100 25");
}

/// "A gate that can be got past by being slow is not a gate" has to survive
/// the budget too, so the budget never skips one. The runner is fake and the
/// elapsed time deterministic: spawning a real process is the runner's own
/// test surface, not this policy's.
#[test]
fn a_gate_is_never_skipped_by_an_exhausted_budget() {
    let dir = tempfile::tempdir().unwrap();
    // The command is never spawned: the fake runner below answers instead.
    let ctx = HookContext::with_hooks(
        hooks_from(
            r#"{"version":1,"hooks":[{"id":"probe","events":["pre-tool-use"],"command":["/nonexistent/hook"]}]}"#,
        ),
        dir.path().to_path_buf(),
    )
    .with_turn_budget(100);
    // Everything the budget allowed is already gone.
    ctx.spent_ms.set(10_000);

    let calls = Cell::new(0usize);
    let outcome = ctx.fire_with_runner(
        HookEvent::PreToolUse,
        HookSubject::tool("execute", "{}"),
        || json!({}),
        &never_cancelled,
        |_hook, _payload, _env, _cwd, timeout, _cancel| {
            // An exhausted budget still gives a gate its minimum slice.
            assert_eq!(timeout, Duration::from_millis(config::MIN_TIMEOUT_MS));
            calls.set(calls.get() + 1);
            (
                runner::HookRun::Answered(runner::HookResponse {
                    decision: Some("deny".to_string()),
                    reason: Some("still watching".to_string()),
                    ..Default::default()
                }),
                Duration::from_millis(1),
            )
        },
    );

    assert_eq!(calls.get(), 1, "the gate must still have run");
    let (hook, reason) = outcome.denied().expect("the gate must still have run");
    assert_eq!(hook, "probe");
    assert!(reason.contains("still watching"), "{reason}");
}

/// An observer carries no decision, so cutting it is the cost the user
/// chose when they set a budget.
#[test]
fn an_exhausted_budget_skips_an_observer() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let ctx = timed_context(
        &dir,
        r#"["post-tool-use"]"#,
        5_000,
        &format!("touch {}\n", marker.display()),
    )
    .with_turn_budget(1_000);
    ctx.spent_ms.set(1_000);

    let outcome = ctx.fire(
        HookEvent::PostToolUse,
        HookSubject::tool("execute", "{}"),
        || json!({}),
        &never_cancelled,
    );

    assert_eq!(outcome.decision, HookDecision::Continue);
    assert!(!marker.exists(), "the observer should not have run");
}

/// The budget shortens a gate instead of skipping it, and a shortened gate
/// that times out lands on the existing fail-closed rule. The timeout itself
/// is fake: real process killing is covered in `runner.rs`.
#[test]
fn a_gate_shortened_by_the_budget_that_times_out_denies() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = HookContext::with_hooks(
        hooks_from(
            r#"{"version":1,"hooks":[{"id":"probe","events":["pre-tool-use"],"timeout_ms":60000,"command":["/nonexistent/hook"]}]}"#,
        ),
        dir.path().to_path_buf(),
    )
    .with_turn_budget(1_000);
    ctx.spent_ms.set(850);

    let outcome = ctx.fire_with_runner(
        HookEvent::PreToolUse,
        HookSubject::tool("execute", "{}"),
        || json!({}),
        &never_cancelled,
        |_hook, _payload, _env, _cwd, timeout, _cancel| {
            assert_eq!(timeout, Duration::from_millis(150));
            (
                runner::HookRun::Failed("timed out after 150ms".to_string()),
                Duration::from_millis(150),
            )
        },
    );

    let (_, reason) = outcome.denied().expect("a timed-out gate must refuse");
    assert!(reason.contains("timed out after 150ms"), "{reason}");
    assert!(reason.contains(HOOK_TURN_BUDGET_KEY), "{reason}");
}

/// An observer carries no decision, so cutting it is the cost the user chose
/// when they set a budget. The elapsed time is fake and deterministic: the
/// first fire consumes the budget through the real `fire` policy, and the
/// rest are skipped without the runner being called.
#[test]
fn the_budget_accumulates_across_fires() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = HookContext::with_hooks(
        hooks_from(
            r#"{"version":1,"hooks":[{"id":"probe","events":["post-tool-use"],"timeout_ms":1000,"command":["/nonexistent/hook"]}]}"#,
        ),
        dir.path().to_path_buf(),
    )
    .with_turn_budget(200);

    let calls = Cell::new(0usize);
    for _ in 0..3 {
        ctx.fire_with_runner(
            HookEvent::PostToolUse,
            HookSubject::tool("execute", "{}"),
            || json!({}),
            &never_cancelled,
            |_hook, _payload, _env, _cwd, _timeout, _cancel| {
                calls.set(calls.get() + 1);
                (
                    runner::HookRun::Answered(runner::HookResponse::default()),
                    Duration::from_millis(200),
                )
            },
        );
    }

    // The first run spends the budget; the rest are skipped.
    assert_eq!(calls.get(), 1, "spent_ms = {}", ctx.spent_ms.get());
    assert_eq!(ctx.spent_ms.get(), 200);
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
        HookSubject::tool("edit", "{}"),
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
fn the_turn_budget_is_unlimited_unless_asked_for() {
    let _lock = crate::chatgpt::tool::execute::tests::env_lock();
    let mut proxy = crate::test_support::TestShellProxy::default();
    assert_eq!(
        config::turn_budget_ms(&mut proxy as &mut dyn ShellProxy).unwrap(),
        None
    );

    for (value, expected) in [("2500", Some(2_500)), ("0", None), ("  ", None)] {
        proxy
            .vars
            .insert(HOOK_TURN_BUDGET_KEY.to_string(), value.to_string());
        assert_eq!(
            config::turn_budget_ms(&mut proxy as &mut dyn ShellProxy).unwrap(),
            expected,
            "{value}"
        );
    }

    // A budget no hook could finish inside is a typo, not a way to turn
    // hooks off; `0` is the way to turn them off.
    for value in ["50", "soon"] {
        proxy
            .vars
            .insert(HOOK_TURN_BUDGET_KEY.to_string(), value.to_string());
        assert!(
            config::turn_budget_ms(&mut proxy as &mut dyn ShellProxy).is_err(),
            "{value}"
        );
    }
}

/// `AI_CHAT_HOOKS=off` is the documented way out of a broken hook setup, so
/// a malformed budget must not be able to refuse the chat past it.
#[test]
fn a_malformed_budget_does_not_refuse_a_chat_with_hooks_switched_off() {
    let _lock = crate::chatgpt::tool::execute::tests::env_lock();
    let mut proxy = crate::test_support::TestShellProxy::default();
    proxy
        .vars
        .insert(HOOK_TURN_BUDGET_KEY.to_string(), "2s".to_string());

    // On its own the value is an error...
    assert!(config::turn_budget_ms(&mut proxy as &mut dyn ShellProxy).is_err());

    // ...but with nothing to budget, there is nothing to refuse.
    proxy
        .vars
        .insert(HOOKS_ENABLED_KEY.to_string(), "off".to_string());
    let ctx = HookContext::load(&mut proxy).expect("hooks are off");
    assert!(ctx.turn_budget_ms.is_none());
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
