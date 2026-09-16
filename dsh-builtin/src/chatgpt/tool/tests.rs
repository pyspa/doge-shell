use super::*;
use serde_json::json;
use tempfile::tempdir;

use crate::test_support::TestShellProxy;
type NoopProxy = TestShellProxy;

/// A workspace member must be able to see the workspace.
#[test]
fn tool_roots_reach_the_project_root_from_a_subdirectory() {
    let dir = tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    let member = root.join("crates/inner");
    std::fs::create_dir_all(&member).unwrap();

    assert!(is_path_within_tool_roots(&root.join("Cargo.toml"), &member));
}

/// The member carries its own `Cargo.toml`, so stopping at the nearest
/// marker left the workspace file one level up out of reach.
#[test]
fn tool_roots_climb_past_a_member_that_is_itself_a_project() {
    let dir = tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    let member = root.join("member");
    std::fs::create_dir_all(&member).unwrap();
    std::fs::write(member.join("Cargo.toml"), "[package]\n").unwrap();

    assert!(is_path_within_tool_roots(&root.join("Cargo.toml"), &member));
}

/// A dotfiles repository in `$HOME` makes `find_project_root` answer
/// `$HOME` for any plain directory beneath it. Widening to that answer put
/// `~/.ssh` and `~/.aws` inside the sandbox of a shell started in, say,
/// `~/scratch`.
///
/// The working directory itself is a root either way - that is the original
/// contract, and running `!` from `$HOME` has always meant that much. What
/// must not happen is *reaching* `$HOME` from somewhere below it.
#[test]
fn widening_never_climbs_out_into_the_home_directory() {
    let _lock = execute::tests::env_lock();
    let fake_home = tempdir().unwrap();
    let home = std::fs::canonicalize(fake_home.path()).unwrap();
    std::fs::create_dir(home.join(".git")).unwrap();

    let scratch = home.join("scratch");
    std::fs::create_dir(&scratch).unwrap();

    // SAFETY: single-threaded under the shared env lock.
    let previous = std::env::var_os("HOME");
    unsafe { std::env::set_var("HOME", &home) };
    let root = workspace_root(&scratch);
    let reaches_home = is_path_within_tool_roots(&home.join(".ssh"), &scratch);
    match previous {
        Some(value) => unsafe { std::env::set_var("HOME", value) },
        None => unsafe { std::env::remove_var("HOME") },
    }

    assert_eq!(root, scratch, "widening must stop below $HOME");
    assert!(!reaches_home, "$HOME must stay outside the sandbox");
}

/// Widening stops at the project, not at the home directory.
#[test]
fn tool_roots_do_not_reach_above_the_project() {
    let dir = tempdir().unwrap();
    let outside = std::fs::canonicalize(dir.path()).unwrap();
    let root = outside.join("project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(outside.join("secrets.txt"), "no").unwrap();

    assert!(!is_path_within_tool_roots(
        &outside.join("secrets.txt"),
        &root
    ));
}

#[test]
fn test_truncation_short() {
    let short = "Short output";
    assert_eq!(truncate_output(short.to_string()), short);
}

#[test]
fn test_truncation_exact() {
    let exact = "a".repeat(MAX_OUTPUT_LENGTH);
    assert_eq!(truncate_output(exact.clone()), exact);
}

#[test]
fn truncate_output_keeps_the_tail() {
    // The tail carries the compiler error / test summary the model must see.
    let long = format!("{}{}", "H".repeat(MAX_OUTPUT_LENGTH), "TAIL-MARKER");
    let truncated = truncate_output(long);

    assert!(truncated.starts_with("HHH"));
    assert!(truncated.ends_with("TAIL-MARKER"));
    assert!(truncated.contains("truncated"));
    assert!(truncated.len() < MAX_OUTPUT_LENGTH + 64);
}

/// The trimmed JSON can still be too big for a chatty command (five 2048
/// char streams comfortably clear the 8192 char cap on their own), and the
/// final fallback used to drop `exit_code`/`status` along with everything
/// else - `result_failed` then had nothing to read and reported a failed
/// command as a Success.
#[test]
fn truncate_output_final_fallback_keeps_exit_code_and_status() {
    // Each field over 2048 chars is capped to ~2048 by `trim`, so enough
    // large fields (not larger fields) are what pushes the *trimmed*
    // value itself past `MAX_OUTPUT_LENGTH` and into the final fallback.
    let huge = "E".repeat(4096);
    let output = serde_json::json!({
        "exit_code": 1,
        "status": "failed",
        "job_id": "job-1",
        "stdout": huge.clone(),
        "stderr": huge.clone(),
        "note": huge.clone(),
        "extra_a": huge.clone(),
        "extra_b": huge,
    })
    .to_string();
    assert!(output.len() > MAX_OUTPUT_LENGTH);

    let truncated = truncate_output(output);
    assert!(truncated.len() <= MAX_OUTPUT_LENGTH + 512);

    let value: Value = serde_json::from_str(&truncated).expect("still valid JSON");
    assert_eq!(value["exit_code"], Value::from(1));
    assert_eq!(value["status"], Value::from("failed"));
    assert_eq!(value["job_id"], Value::from("job-1"));
    assert_eq!(value["output_truncated"], Value::from(true));
    assert!(
        result_failed(&truncated),
        "a truncated failing result must still read as a failure"
    );
}

/// A malformed or hostile tool response could set `status`/`job_id` to
/// something far longer than the short control values they are meant to
/// hold. Carrying those fields over verbatim (as an earlier version of
/// this fallback did) could push the whole fallback back over
/// `MAX_OUTPUT_LENGTH` on top of the fixed preview budget - reopening the
/// exact "result no longer fits" problem the fallback exists to solve.
#[test]
fn truncate_output_final_fallback_stays_bounded_even_with_oversized_status_fields() {
    let huge = "E".repeat(4096);
    let output = serde_json::json!({
        "exit_code": 1,
        "status": "failed ".repeat(1000),
        "job_id": "job-".repeat(1000),
        "stdout": huge,
    })
    .to_string();
    assert!(output.len() > MAX_OUTPUT_LENGTH);

    let truncated = truncate_output(output);
    assert!(
        truncated.len() <= MAX_OUTPUT_LENGTH + 512,
        "fallback grew unbounded: {} chars",
        truncated.len()
    );

    let value: Value = serde_json::from_str(&truncated).expect("still valid JSON");
    assert_eq!(value["exit_code"], Value::from(1));
    assert!(result_failed(&truncated));
}

#[test]
fn execute_tool_call_returns_parseable_json_after_the_global_cap() {
    // The global cap runs after the tool, so it must not corrupt a
    // structured result on its way back to the model.
    //
    // `EXECUTE_TOOL_ENV_ALLOWLIST` is process-global and wins over the
    // proxy's list, so this has to hold the same lock as the tests that set
    // it or `ls` stops being allowed halfway through the run.
    let _lock = super::execute::tests::ENV_LOCK.lock().unwrap();
    let _env_guard =
        super::execute::tests::EnvGuard::set(super::execute::EXECUTE_TOOL_ENV_ALLOWLIST, "ls");

    let dir = tempdir().unwrap();
    for index in 0..400 {
        std::fs::write(dir.path().join(format!("f-{index:0>50}")), b"x").unwrap();
    }

    let mut proxy = NoopProxy {
        current_dir: std::env::current_dir().unwrap(),
        execute_allowlist: vec!["ls".to_string()],
        confirm_result: true,
        ..NoopProxy::default()
    };

    let tool_call = json!({
        "function": {
            "name": "execute",
            "arguments": format!("{{\"command\":\"ls -R {}\"}}", dir.path().display())
        }
    });

    let result = execute_tool_call(
        &tool_call,
        &Arc::new(RwLock::new(crate::chatgpt::McpManager::default())),
        &HookContext::disabled(),
        &mut proxy,
    )
    .unwrap();

    assert!(result.content.len() <= MAX_OUTPUT_LENGTH + 128);
    serde_json::from_str::<Value>(&result.content).expect("tool result must stay valid JSON");
    assert_eq!(result.outcome, ToolOutcome::Success);
}

#[test]
fn tool_argument_log_redacts_secret_like_values() {
    let args = r#"{"path":"config.txt","contents":"API_KEY=secret Authorization: Bearer token"}"#;
    let redacted = redact_tool_arguments(args);

    assert!(redacted.contains("API_KEY=***"));
    assert!(redacted.contains("Authorization: Bearer ***"));
    assert!(!redacted.contains("secret"));
    assert!(!redacted.contains("token"));
}

#[test]
fn test_execute_tool_call_unknown_tool() {
    let mut proxy = NoopProxy::default();
    let mcp = Arc::new(RwLock::new(McpManager::load_blocking(vec![])));
    let tool_call = serde_json::json!({
        "function": {
            "name": "unknown_tool",
            "arguments": "{}"
        }
    });

    let result = execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "chat: unsupported tool `unknown_tool`"
    );
}

/// The policy decides, not the call site. `!` used to prompt for every MCP
/// call at every safety level, which `loose` was supposed to switch off and
/// which made "always" unreachable.
#[test]
fn an_allowed_mcp_tool_runs_without_asking() {
    let mut proxy = NoopProxy {
        agent_tool_verdict: AgentCommandVerdict::Allowed,
        ..NoopProxy::default()
    };
    let mut inner = McpManager::default();
    inner.insert_test_tool_binding("mcp__test__tool");
    let mcp = Arc::new(RwLock::new(inner));
    let tool_call = serde_json::json!({
        "function": {"name": "mcp__test__tool", "arguments": "{}"}
    });

    // No binding is actually connected, so the call fails after the gate -
    // what matters is that the gate did not ask.
    let error =
        execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy).unwrap_err();
    assert_eq!(error.outcome, ToolOutcome::Failure);
    assert_eq!(proxy.confirm_calls, 0);
}

#[test]
fn a_denied_mcp_tool_is_refused_without_asking() {
    let mut proxy = NoopProxy {
        agent_tool_verdict: AgentCommandVerdict::Denied("policy says no".to_string()),
        ..NoopProxy::default()
    };
    let mut inner = McpManager::default();
    inner.insert_test_tool_binding("mcp__test__tool");
    let mcp = Arc::new(RwLock::new(inner));
    let tool_call = serde_json::json!({
        "function": {"name": "mcp__test__tool", "arguments": "{}"}
    });

    let err =
        execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy).unwrap_err();
    assert!(err.to_string().contains("policy says no"));
    assert_eq!(proxy.confirm_calls, 0);
}

/// "always" was unreachable while this path used the bool `confirm_action`.
#[test]
fn an_always_answer_is_remembered_for_the_session() {
    let mut proxy = NoopProxy {
        approval_decision: Some(ApprovalDecision::AllowAlways),
        ..NoopProxy::default()
    };
    let mut inner = McpManager::default();
    inner.insert_test_tool_binding("mcp__test__tool");
    let mcp = Arc::new(RwLock::new(inner));
    let tool_call = serde_json::json!({
        "function": {"name": "mcp__test__tool", "arguments": "{}"}
    });

    let _ = execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy);
    assert_eq!(proxy.agent_session_allowlist, vec!["mcp:mcp__test__tool"]);
}

#[test]
fn execute_tool_call_requires_confirmation_for_mcp_tool() {
    let mut proxy = NoopProxy::default();
    let mut inner = McpManager::default();
    inner.insert_test_tool_binding("mcp__test__tool");
    let mcp = Arc::new(RwLock::new(inner));
    let tool_call = serde_json::json!({
        "function": {
            "name": "mcp__test__tool",
            "arguments": "{}"
        }
    });

    let result = execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy).unwrap();

    assert_eq!(result.content, "MCP tool execution cancelled by user.");
    assert_eq!(result.outcome, ToolOutcome::Failure);
}

/// A hook context whose single hook prints `body` on every tool call.
fn hook_context(dir: &tempfile::TempDir, body: &str) -> HookContext {
    // Run as `sh <path>` rather than exec'ing a file this process just
    // wrote: a concurrent test's `fork` holds the write descriptor open and
    // the kernel answers with ETXTBSY.
    let path = dir.path().join("hook.sh");
    std::fs::write(&path, format!("{body}\n")).unwrap();

    let config = format!(
        r#"{{"version":1,"hooks":[{{"id":"gatekeeper","events":["pre-tool-use","post-tool-use"],"command":["sh","{}"]}}]}}"#,
        path.display()
    );
    HookContext::with_hooks(
        hooks::config::parse(&config).expect("test hook config"),
        dir.path().to_path_buf(),
    )
}

/// Like `hook_context`, but with a `match` clause the caller chooses.
fn matched_hook_context(dir: &tempfile::TempDir, matcher: &str, body: &str) -> HookContext {
    let path = dir.path().join("hook.sh");
    std::fs::write(&path, format!("{body}\n")).unwrap();

    let config = format!(
        r#"{{"version":1,"hooks":[{{"id":"gatekeeper","events":["pre-tool-use"],"match":{matcher},"command":["sh","{}"]}}]}}"#,
        path.display()
    );
    HookContext::with_hooks(
        hooks::config::parse(&config).expect("test hook config"),
        dir.path().to_path_buf(),
    )
}

fn execute_call(command: &str) -> Value {
    serde_json::json!({
        "id": "call_1",
        "function": {
            "name": "execute",
            "arguments": serde_json::json!({ "command": command }).to_string(),
        }
    })
}

/// `dogesh` puts every command through `execute`, so a hook watching `rm` used
/// to pay its timeout on every `ls`. `programs` is what makes it not.
#[test]
fn a_hook_matching_on_the_command_narrows_to_one_call() {
    let dir = tempdir().unwrap();
    let hooks = matched_hook_context(
        &dir,
        r#"{"tools":["execute"],"programs":["rm"]}"#,
        r#"echo '{"decision":"deny","reason":"no removals here"}'"#,
    );
    let mcp = Arc::new(RwLock::new(McpManager::default()));

    // The tool the hook does not care about runs untouched - and never even
    // starts the hook process.
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        agent_verdict: AgentCommandVerdict::Allowed,
        ..TestShellProxy::default()
    };
    let allowed = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();
    assert_eq!(allowed.outcome, ToolOutcome::Success);

    // The one it does care about is stopped before the policy is asked.
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        agent_verdict: AgentCommandVerdict::Allowed,
        ..TestShellProxy::default()
    };
    let denied =
        execute_tool_call(&execute_call("rm -rf /tmp/x"), &mcp, &hooks, &mut proxy).unwrap();
    assert_eq!(denied.outcome, ToolOutcome::Failure);
    assert!(
        denied.content.contains("no removals here"),
        "{}",
        denied.content
    );

    // A different command through the same tool is not the hook's business.
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        agent_verdict: AgentCommandVerdict::Allowed,
        ..TestShellProxy::default()
    };
    let other = execute_tool_call(&execute_call("true"), &mcp, &hooks, &mut proxy).unwrap();
    assert_eq!(other.outcome, ToolOutcome::Success);
}

fn ls_call() -> Value {
    serde_json::json!({
        "id": "call_1",
        "function": {"name": "ls", "arguments": "{\"path\":\".\"}"}
    })
}

#[test]
fn pre_tool_use_deny_skips_execution() {
    let dir = tempdir().unwrap();
    let hooks = hook_context(&dir, r#"echo '{"decision":"deny","reason":"not here"}'"#);
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        confirm_result: true,
        ..TestShellProxy::default()
    };
    let mcp = Arc::new(RwLock::new(McpManager::default()));

    let result = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

    assert_eq!(result.outcome, ToolOutcome::Failure);
    assert!(
        result.content.contains("Blocked by hook `gatekeeper`"),
        "{}",
        result.content
    );
    assert!(result.content.contains("not here"), "{}", result.content);
}

/// A hook's `ask` has to reach the user even where the safety policy would
/// have said nothing at all.
#[test]
fn pre_tool_use_ask_requires_approval_even_when_the_policy_allows() {
    let dir = tempdir().unwrap();
    let hooks = hook_context(&dir, r#"echo '{"decision":"ask","reason":"double-check"}'"#);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        agent_verdict: AgentCommandVerdict::Allowed,
        confirm_counter: Some(calls.clone()),
        confirm_result: true,
        ..TestShellProxy::default()
    };
    let mcp = Arc::new(RwLock::new(McpManager::default()));

    let result = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(result.outcome, ToolOutcome::Success);
}

#[test]
fn pre_tool_use_ask_denied_by_user_does_not_run_the_tool() {
    let dir = tempdir().unwrap();
    let hooks = hook_context(&dir, r#"echo '{"decision":"ask","reason":"double-check"}'"#);
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        agent_verdict: AgentCommandVerdict::Allowed,
        confirm_result: false,
        ..TestShellProxy::default()
    };
    let mcp = Arc::new(RwLock::new(McpManager::default()));

    let result = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

    assert_eq!(result.outcome, ToolOutcome::Failure);
    assert!(
        result.content.contains("Blocked by hook"),
        "{}",
        result.content
    );
}

/// The `hook:` prefix keeps this out of the box `execute` and MCP calls use,
/// so an "always" on one never answers the other's question.
#[test]
fn pre_tool_use_ask_uses_its_own_approval_key() {
    let dir = tempdir().unwrap();
    let hooks = hook_context(&dir, r#"echo '{"decision":"ask","reason":"double-check"}'"#);
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        agent_verdict: AgentCommandVerdict::Allowed,
        approval_decision: Some(ApprovalDecision::AllowAlways),
        ..TestShellProxy::default()
    };
    let mcp = Arc::new(RwLock::new(McpManager::default()));

    execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

    assert!(
        proxy
            .agent_session_allowlist
            .contains(&"hook:gatekeeper:ls".to_string()),
        "{:?}",
        proxy.agent_session_allowlist
    );
    // Not the plain tool name, which is what `execute` remembers.
    assert!(!proxy.agent_session_allowlist.contains(&"ls".to_string()));
}

/// The tool has already run, so this cannot undo it - but the model must not
/// read the result as a success.
#[test]
fn post_tool_use_deny_marks_the_result_failed() {
    let dir = tempdir().unwrap();
    let hooks = hook_context(
        &dir,
        r#"[ "$DOGESH_HOOK_EVENT" = post-tool-use ] && echo '{"decision":"deny","reason":"leaked a path"}'
exit 0"#,
    );
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        confirm_result: true,
        ..TestShellProxy::default()
    };
    let mcp = Arc::new(RwLock::new(McpManager::default()));

    let result = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

    assert_eq!(result.outcome, ToolOutcome::Failure);
    assert!(
        result.content.contains("Rejected by hook"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("leaked a path"),
        "{}",
        result.content
    );
}

#[test]
fn post_tool_use_additional_context_reaches_the_model() {
    let dir = tempdir().unwrap();
    let hooks = hook_context(
        &dir,
        r#"[ "$DOGESH_HOOK_EVENT" = post-tool-use ] && echo '{"additional_context":"repo policy applies"}'
exit 0"#,
    );
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        confirm_result: true,
        ..TestShellProxy::default()
    };
    let mcp = Arc::new(RwLock::new(McpManager::default()));

    let result = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

    assert_eq!(result.outcome, ToolOutcome::Success);
    assert!(
        result.content.ends_with("repo policy applies"),
        "{}",
        result.content
    );
}

/// A hook that adds context on `pre-tool-use` was advertised as reaching the
/// model, and reached nothing.
#[test]
fn pre_tool_use_additional_context_reaches_the_model() {
    let dir = tempdir().unwrap();
    let hooks = hook_context(
        &dir,
        r#"[ "$DOGESH_HOOK_EVENT" = pre-tool-use ] && echo '{"additional_context":"read-only day"}'
exit 0"#,
    );
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        confirm_result: true,
        ..TestShellProxy::default()
    };
    let mcp = Arc::new(RwLock::new(McpManager::default()));

    let result = execute_tool_call(&ls_call(), &mcp, &hooks, &mut proxy).unwrap();

    assert!(
        result.content.starts_with("read-only day"),
        "{}",
        result.content
    );
    assert_eq!(result.outcome, ToolOutcome::Success);
}

/// An audit hook that pairs pre with post was left holding an unmatched open
/// event for exactly the calls it most wants to see.
#[test]
fn post_tool_use_fires_for_a_failing_tool() {
    let dir = tempdir().unwrap();
    let log = dir.path().join("seen.log");
    let hooks = hook_context(
        &dir,
        &format!("printf '%s\\n' \"$DOGESH_HOOK_EVENT\" >> {}", log.display()),
    );
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        confirm_result: true,
        ..TestShellProxy::default()
    };
    let mcp = Arc::new(RwLock::new(McpManager::default()));
    let failing = serde_json::json!({
        "id": "call_1",
        "function": {"name": "ls", "arguments": "{\"path\":\"../outside\"}"}
    });

    let error = execute_tool_call(&failing, &mcp, &hooks, &mut proxy)
        .expect_err("the path is outside the allowed roots");

    assert_eq!(error.outcome, ToolOutcome::Failure);
    let seen = std::fs::read_to_string(&log).unwrap();
    assert!(seen.contains("pre-tool-use"), "{seen}");
    assert!(seen.contains("post-tool-use"), "{seen}");
}

/// The exemption exists so a repository that ignores `.dogesh/` can still have
/// its project skills read. It stops there: writing one goes through
/// `skill_manage`, which validates what plain `edit` would not.
#[test]
fn a_gitignored_skill_directory_is_readable_but_not_writable() {
    let dir = tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(
        root.join(".gitignore"),
        ".dogesh/
",
    )
    .unwrap();
    let skill = root.join(".dogesh/skills/demo");
    std::fs::create_dir_all(&skill).unwrap();
    let file = skill.join("SKILL.md");
    std::fs::write(&file, "---\ndescription: d\n---\n").unwrap();

    assert!(reject_gitignored_read_path(&file, &root, "SKILL.md").is_ok());
    let refused = reject_gitignored_path(&file, &root, "SKILL.md")
        .expect_err("writing into an ignored directory stays refused");
    assert!(refused.contains("ignored by .gitignore"), "{refused}");
}

type CwdProxy = TestShellProxy;

#[test]
fn a_new_file_resolves_without_a_trailing_separator() {
    let dir = tempdir().unwrap();
    let mut proxy = CwdProxy {
        current_dir: dir.path().to_path_buf(),
        ..CwdProxy::default()
    };

    // `Path::join` on an empty path appends a separator, so this used to
    // come back as `notes.txt/` and every `edit` that created a file
    // failed with ENOENT.
    let resolved = resolve_tool_path("notes.txt", &mut proxy).unwrap();
    assert_eq!(resolved.file_name().unwrap(), "notes.txt");
    assert!(!resolved.to_string_lossy().ends_with('/'));

    let nested = resolve_tool_path("a/b/notes.txt", &mut proxy).unwrap();
    assert!(nested.ends_with("a/b/notes.txt"));
    assert!(!nested.to_string_lossy().ends_with('/'));
}

#[test]
fn resolve_tool_path_rejects_parent_traversal() {
    let dir = tempdir().unwrap();
    let mut proxy = CwdProxy {
        current_dir: dir.path().to_path_buf(),
        ..CwdProxy::default()
    };
    let result = resolve_tool_path("../outside.txt", &mut proxy);
    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn resolve_tool_path_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let base = tempdir().unwrap();
    let outside = tempdir().unwrap();
    std::fs::create_dir_all(base.path().join("inside")).unwrap();
    symlink(outside.path(), base.path().join("inside/link_out")).unwrap();

    let mut proxy = CwdProxy {
        current_dir: base.path().to_path_buf(),
        ..CwdProxy::default()
    };
    let result = resolve_tool_path("inside/link_out/pwned.txt", &mut proxy);
    assert!(result.is_err());
}
