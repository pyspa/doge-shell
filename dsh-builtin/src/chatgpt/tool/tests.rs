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

/// The job tools are the half of the task toolbox an interactive turn needs:
/// a `!` chat starts managed commands too, and without these it could start
/// one and never hear how it went.
#[test]
fn job_status_works_without_an_agent_task() {
    let mut proxy = NoopProxy::default();
    let mcp = Arc::new(RwLock::new(McpManager::load_blocking(vec![])));
    let tool_call = json!({
        "function": {
            "name": "job_status",
            "arguments": "{\"job_id\":\"no-such-job\"}"
        }
    });

    let error = execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy)
        .expect_err("an unknown job is still an error");

    // The point is *which* error: "unknown job" means the interactive registry
    // answered, where "requires an agent task" would mean the gate refused.
    let error = error.to_string();
    assert!(error.contains("unknown job"), "{error}");
    assert!(!error.contains("agent task"), "{error}");
}

/// The rest of the task toolbox stays behind the gate: `task_verify` records
/// against criteria and `tool_search` exists to find MCP definitions that an
/// interactive prompt already carries in full.
#[test]
fn the_task_only_tools_still_require_an_agent_task() {
    let mcp = Arc::new(RwLock::new(McpManager::load_blocking(vec![])));

    for (name, arguments) in [
        (
            "task_verify",
            "{\"criterion\":0,\"evidence_event\":1,\"explanation\":\"x\"}",
        ),
        ("task_plan", "{\"plan\":[\"x\"],\"progress\":\"x\"}"),
        ("tool_search", "{\"query\":\"x\"}"),
    ] {
        let mut proxy = NoopProxy::default();
        let tool_call = json!({ "function": { "name": name, "arguments": arguments } });
        let error = execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy)
            .expect_err("{name} should need a task")
            .to_string();
        assert!(error.contains("requires an agent task"), "{name}: {error}");
    }
}

/// Both entry points advertise the same three tools under the same names, so
/// a skill or a habit learned in one keeps working in the other.
#[test]
fn the_job_tools_are_spelled_the_same_in_both_toolboxes() {
    let interactive: Vec<String> = job_definitions()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
        .collect();
    let task: Vec<String> = agent_definitions()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
        .collect();

    assert_eq!(interactive, ["job_status", "job_output", "job_cancel"]);
    for name in &interactive {
        assert!(
            task.contains(name),
            "{name} is missing from the task toolbox"
        );
    }
}

/// Group discovery is metadata, not a side effect: no approval question.
#[test]
fn mcp_list_groups_runs_without_approval() {
    let mut proxy = NoopProxy::default();
    let mut inner = McpManager::default();
    inner.insert_test_tool("github", "list_issues");
    let mcp = Arc::new(RwLock::new(inner));
    let tool_call = json!({ "function": { "name": "mcp_list_groups", "arguments": "{}" } });

    let result = execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy).unwrap();

    assert_eq!(result.outcome, ToolOutcome::Success);
    assert_eq!(proxy.confirm_calls, 0);
    let catalogue: Value = serde_json::from_str(&result.content).expect("valid JSON");
    assert_eq!(catalogue["groups"][0]["name"], "github");
    assert_eq!(catalogue["groups"][0]["tool_count"], 1);
}

/// Activation flows through the same dispatch as every builtin: no approval
/// question, and the manager state the next request reads is changed.
#[test]
fn mcp_load_group_activates_through_dispatch_without_approval() {
    let mut proxy = NoopProxy::default();
    let mut inner = McpManager::default();
    inner.insert_test_tool("github", "list_issues");
    inner.disable_group("github").unwrap();
    let mcp = Arc::new(RwLock::new(inner));
    let tool_call =
        json!({ "function": { "name": "mcp_load_group", "arguments": r#"{"group":"github"}"# } });

    let result = execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy).unwrap();

    assert_eq!(result.outcome, ToolOutcome::Success);
    assert_eq!(proxy.confirm_calls, 0);
    assert!(mcp.read().is_group_enabled("github"));

    // Unknown groups come back as a model-readable error, not a stopped turn.
    let missing =
        json!({ "function": { "name": "mcp_load_group", "arguments": r#"{"group":"nope"}"# } });
    let error =
        execute_tool_call(&missing, &mcp, &HookContext::disabled(), &mut proxy).unwrap_err();
    assert!(
        error.to_string().contains("Unknown MCP tool group"),
        "{error}"
    );
}

/// Meta tools ride along wherever MCP exists but never where there is
/// nothing to discover; full schemas stay interactive-only.
#[test]
fn turn_definitions_gate_meta_and_full_schemas() {
    let empty = McpManager::default();
    assert!(mcp_turn_definitions(&empty, true).is_empty());
    assert!(mcp_turn_definitions(&empty, false).is_empty());

    let mut inner = McpManager::default();
    inner.insert_test_tool("github", "list_issues");
    let names = |tools: Vec<Value>| {
        tools
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(mcp_turn_definitions(&inner, true)),
        vec![
            "mcp_list_groups",
            "mcp_load_group",
            "mcp__github__list_issues"
        ]
    );
    assert_eq!(
        names(mcp_turn_definitions(&inner, false)),
        vec!["mcp_list_groups", "mcp_load_group"]
    );
}

fn lazy_manager() -> McpManager {
    let mut inner = McpManager::default();
    inner.insert_test_tool("github", "list_issues");
    inner.insert_test_tool("github", "get_issue");
    inner.insert_test_tool("filesystem", "read_file");
    inner.insert_test_tool("slack", "post_message");
    for group in ["github", "filesystem", "slack"] {
        inner.disable_group(group).unwrap();
    }
    inner
}

fn definition_names(tools: &[Value]) -> Vec<String> {
    let mut names: Vec<String> = tools
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
        .collect();
    names.sort_unstable();
    names
}

/// With every group inactive, the initial interactive definitions carry zero
/// actual MCP tools - only the two discovery meta tools.
#[test]
fn initial_interactive_definitions_hide_every_inactive_group() {
    let manager = lazy_manager();

    assert_eq!(
        definition_names(&mcp_turn_definitions(&manager, true)),
        vec!["mcp_list_groups", "mcp_load_group"]
    );
}

/// Loading one group exposes exactly its tools; the others stay hidden.
#[test]
fn interactive_definitions_isolate_the_active_group() {
    let manager = lazy_manager();
    manager.enable_group("github").unwrap();

    assert_eq!(
        definition_names(&mcp_turn_definitions(&manager, true)),
        vec![
            "mcp__github__get_issue",
            "mcp__github__list_issues",
            "mcp_list_groups",
            "mcp_load_group",
        ]
    );
}

/// Several active groups all surface; an inactive one still does not.
#[test]
fn interactive_definitions_combine_multiple_active_groups() {
    let manager = lazy_manager();
    manager.enable_group("github").unwrap();
    manager.enable_group("filesystem").unwrap();

    assert_eq!(
        definition_names(&mcp_turn_definitions(&manager, true)),
        vec![
            "mcp__filesystem__read_file",
            "mcp__github__get_issue",
            "mcp__github__list_issues",
            "mcp_list_groups",
            "mcp_load_group",
        ]
    );
}

/// Re-enabling an already active group never duplicates a definition.
#[test]
fn reloading_a_group_yields_no_duplicate_definitions() {
    let manager = lazy_manager();
    manager.enable_group("github").unwrap();
    let loaded = mcp_turn_definitions(&manager, true);

    manager.enable_group("github").unwrap();
    let reloaded = mcp_turn_definitions(&manager, true);

    assert_eq!(reloaded.len(), loaded.len());
    let names = definition_names(&reloaded);
    let mut deduped = names.clone();
    deduped.dedup();
    assert_eq!(names, deduped);
}

/// Agent turns stay meta-only no matter the exposure: their discovery path
/// is `tool_search`, unchanged by interactive lazy loading.
#[test]
fn agent_definitions_ignore_group_exposure() {
    let manager = lazy_manager();
    manager.enable_group("github").unwrap();
    manager.enable_group("filesystem").unwrap();

    assert_eq!(
        definition_names(&mcp_turn_definitions(&manager, false)),
        vec!["mcp_list_groups", "mcp_load_group"]
    );
}

/// Fixed Tool Search v2 surface: three servers with realistic descriptions
/// and parameter schemas, so ranking is exercised over every search field.
fn search_bench_manager() -> McpManager {
    let mut manager = McpManager::default();
    manager.insert_test_tool_full(
        "github",
        "search_issues",
        "Search GitHub issues using keywords and filters.",
        json!({"type": "object", "properties": {
            "query": {"type": "string", "description": "The search keywords to use"},
            "state": {"type": "string", "description": "Filter by open or closed state"},
            "labels": {"type": "string", "description": "Filter by label names"},
        }}),
    );
    manager.insert_test_tool_full(
        "github",
        "get_issue",
        "Get a single GitHub issue by number.",
        json!({"type": "object", "properties": {
            "issue_number": {"type": "integer", "description": "The issue number to fetch"},
        }}),
    );
    manager.insert_test_tool_full(
        "github",
        "create_issue",
        "Create a new GitHub issue.",
        json!({"type": "object", "properties": {
            "title": {"type": "string", "description": "The issue title"},
            "body": {"type": "string", "description": "The issue body text"},
        }}),
    );
    manager.insert_test_tool_full(
        "github",
        "list_pull_requests",
        "List pull requests in a repository.",
        json!({"type": "object", "properties": {
            "state": {"type": "string", "description": "Filter by open or closed state"},
            "repository": {"type": "string", "description": "The repository full name"},
        }}),
    );
    manager.insert_test_tool_full(
        "github",
        "search_repositories",
        "Search GitHub repositories by keyword.",
        json!({"type": "object", "properties": {
            "query": {"type": "string", "description": "The search keywords to use"},
        }}),
    );
    manager.insert_test_tool_full(
        "filesystem",
        "read_file",
        "Read a file from the filesystem.",
        json!({"type": "object", "properties": {
            "path": {"type": "string", "description": "The file path to read"},
        }}),
    );
    manager.insert_test_tool_full(
        "filesystem",
        "write_file",
        "Write content to a file on the filesystem.",
        json!({"type": "object", "properties": {
            "path": {"type": "string", "description": "The file path to write"},
            "content": {"type": "string", "description": "The content to write"},
        }}),
    );
    manager.insert_test_tool_full(
        "slack",
        "post_message",
        "Send a message to a Slack channel.",
        json!({"type": "object", "properties": {
            "channel": {"type": "string", "description": "The channel to post to"},
            "text": {"type": "string", "description": "The message text to send"},
        }}),
    );
    manager
}

/// The regression benchmark: representative queries with the tool each must
/// find. Every case asserts Top-1 - a near-tie reorder that drops an
/// expected tool is a ranking regression worth hearing about, and the Top-3
/// / Top-5 tallies below show how far it fell.
const SEARCH_BENCH_CASES: &[(&str, &str)] = &[
    ("search github issues", "mcp__github__search_issues"),
    ("get github issue", "mcp__github__get_issue"),
    ("create github issue", "mcp__github__create_issue"),
    ("list pull requests", "mcp__github__list_pull_requests"),
    ("search repository", "mcp__github__search_repositories"),
    ("send slack message", "mcp__slack__post_message"),
    ("read filesystem file", "mcp__filesystem__read_file"),
];

fn ranked_names(manager: &McpManager, query: &str, limit: usize) -> Vec<String> {
    tool_search::search(manager, query, limit)
        .iter()
        .map(|hit| hit.name.clone())
        .collect()
}

#[test]
fn search_benchmark_ranks_the_expected_tool_first() {
    let manager = search_bench_manager();
    let mut failures = Vec::new();
    let mut top1 = 0usize;
    let mut top3 = 0usize;
    let mut top5 = 0usize;
    for (query, expected) in SEARCH_BENCH_CASES {
        let names = ranked_names(&manager, query, 5);
        match names.iter().position(|name| name == expected) {
            Some(rank) => {
                if rank < 1 {
                    top1 += 1;
                }
                if rank < 3 {
                    top3 += 1;
                }
                top5 += 1;
            }
            None => failures.push(format!(
                "query {query:?}: expected {expected} in top 5, got {names:?}"
            )),
        }
    }
    // Cumulative tallies for the implementation report.
    let total = SEARCH_BENCH_CASES.len();
    println!(
        "tool_search benchmark: Top-1 {top1}/{total}, Top-3 {top3}/{total}, Top-5 {top5}/{total}"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(top1, total, "every benchmark query must hit Top-1");
}

/// An exact tool-name query finds that tool even when siblings share every
/// other signal (server, group, description words).
#[test]
fn search_exact_name_match_ranks_first() {
    let manager = search_bench_manager();

    let names = ranked_names(&manager, "mcp__github__get_issue", 5);

    assert_eq!(
        names.first().map(String::as_str),
        Some("mcp__github__get_issue")
    );
}

/// A query matching only a description still discovers the tool.
#[test]
fn search_description_only_match_discovers_the_tool() {
    let manager = search_bench_manager();

    let names = ranked_names(&manager, "keywords filters", 5);

    assert!(
        names.contains(&"mcp__github__search_issues".to_string()),
        "description-only query lost the tool: {names:?}"
    );
}

/// A query naming a parameter concept finds the tool through its schema.
#[test]
fn search_parameter_match_discovers_the_tool() {
    let manager = search_bench_manager();

    let names = ranked_names(&manager, "issue_number", 5);

    assert_eq!(
        names.first().map(String::as_str),
        Some("mcp__github__get_issue")
    );
}

/// Naming a server finds its tools above unrelated servers' tools.
#[test]
fn search_server_match_favours_that_server() {
    let manager = search_bench_manager();

    let names = ranked_names(&manager, "slack", 5);

    assert_eq!(
        names.first().map(String::as_str),
        Some("mcp__slack__post_message")
    );
}

/// Lexical relevance, not semantic understanding: unrelated servers score
/// zero and never outrank a direct match.
#[test]
fn search_never_ranks_unrelated_servers_above_a_direct_match() {
    let manager = search_bench_manager();

    let names = ranked_names(&manager, "github issues", 5);

    assert!(!names.is_empty());
    assert!(
        names.iter().all(|name| name.starts_with("mcp__github__")),
        "unrelated tools leaked above GitHub matches: {names:?}"
    );
}

/// Ranking is a pure function of current metadata: the same query twice is
/// the same list, regardless of map iteration order.
#[test]
fn search_results_are_deterministic_across_calls() {
    let manager = search_bench_manager();

    let first = ranked_names(&manager, "github issue", 5);
    let second = ranked_names(&manager, "github issue", 5);

    assert_eq!(first, second);
}

/// Discoverable is wider than exposed: every group disabled, every tool
/// still searchable - that is what lazy loading is for.
#[test]
fn search_finds_tools_in_inactive_groups_without_activating_them() {
    let manager = search_bench_manager();
    for group in ["github", "filesystem", "slack"] {
        manager.disable_group(group).unwrap();
    }
    assert_eq!(manager.active_tool_count(), 0);

    let hits = tool_search::search(&manager, "github issues", 5);

    assert!(
        hits.iter()
            .any(|hit| hit.name == "mcp__github__search_issues"),
        "inactive groups must stay discoverable"
    );
    assert!(
        hits.iter().all(|hit| !hit.active),
        "hits from disabled groups must report inactive"
    );
    assert_eq!(
        manager.active_tool_count(),
        0,
        "searching must not flip group toggles"
    );
}

/// Hits from enabled groups report themselves active.
#[test]
fn search_marks_active_group_hits_active() {
    let manager = search_bench_manager();

    let hits = tool_search::search(&manager, "github issues", 5);

    assert!(hits.iter().all(|hit| hit.active));
}

/// A disconnected server offers nothing to discover: `mcp_load_group`
/// cannot bring it back without a reconnect, so ranking its tools would
/// teach the model names it cannot use.
#[test]
fn search_excludes_disconnected_servers() {
    let manager = search_bench_manager();
    manager.disconnect("slack").unwrap();

    let names = ranked_names(&manager, "send slack message", 5);

    assert!(
        !names.iter().any(|name| name.starts_with("mcp__slack__")),
        "disconnected server leaked into results: {names:?}"
    );
}

/// Tool-level loading resolves by name without touching group toggles: the
/// tool becomes callable while its group stays inactive.
#[test]
fn tool_definitions_for_loads_without_flipping_group_toggles() {
    let manager = search_bench_manager();
    manager.disable_group("github").unwrap();

    let definitions = manager.tool_definitions_for(&["mcp__github__search_issues".to_string()]);

    assert_eq!(definitions.len(), 1);
    assert_eq!(
        definitions[0]["function"]["name"],
        "mcp__github__search_issues"
    );
    assert!(
        !manager.is_group_enabled("github"),
        "loading one tool must not expose its whole group"
    );
}

/// Unknown, stale, and disconnected names resolve to nothing rather than
/// erroring; the execution path reports the error if one is still called.
#[test]
fn tool_definitions_for_skips_what_no_longer_resolves() {
    let manager = search_bench_manager();
    manager.disconnect("slack").unwrap();

    let definitions = manager.tool_definitions_for(&[
        "mcp__github__search_issues".to_string(),
        "mcp__nope__missing".to_string(),
        "mcp__slack__post_message".to_string(),
    ]);

    assert_eq!(definitions.len(), 1);
    assert_eq!(
        definitions[0]["function"]["name"],
        "mcp__github__search_issues"
    );
}

/// The default limit keeps results small; an explicit limit is honoured up
/// to the ceiling.
#[test]
fn search_applies_default_and_explicit_limits() {
    let mut manager = McpManager::default();
    for index in 0..7 {
        manager.insert_test_tool("misc", &format!("helper_{index}"));
    }

    let defaulted = ranked_names(&manager, "misc", 1000);
    assert_eq!(
        defaulted.len(),
        7,
        "candidates bound the ceiling, not the clamp"
    );

    let hits = tool_search::search(&manager, "misc", tool_search::DEFAULT_LIMIT);
    assert_eq!(hits.len(), tool_search::DEFAULT_LIMIT);

    let names = ranked_names(&manager, "misc", 2);
    assert_eq!(names.len(), 2);
}

/// Results are compact pointers - name, one-line description, server/group,
/// score, availability - never full schemas.
#[test]
fn tool_search_result_is_compact_json() {
    let manager = search_bench_manager();

    let rendered: Value = serde_json::from_str(
        &tool_search::run(&manager, r#"{"query":"search github issues"}"#).unwrap(),
    )
    .expect("valid JSON");

    assert_eq!(rendered["query"], "search github issues");
    assert!(rendered["count"].as_u64().unwrap() >= 1);
    let first = &rendered["results"][0];
    assert_eq!(first["name"], "mcp__github__search_issues");
    assert_eq!(first["server"], "github");
    assert_eq!(first["group"], "github");
    assert_eq!(first["active"], true);
    assert!(first["score"].as_f64().unwrap() > 0.0);
    assert!(
        first["description"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    assert!(
        !rendered.to_string().contains("parameters"),
        "full schemas defeat lazy loading: {rendered}"
    );
}

/// No match is a normal informative result, not an error and not a
/// fabrication.
#[test]
fn tool_search_reports_no_results_plainly() {
    let manager = search_bench_manager();

    let rendered: Value = serde_json::from_str(
        &tool_search::run(&manager, r#"{"query":"gitlab merge train"}"#).unwrap(),
    )
    .expect("valid JSON");

    assert_eq!(rendered["count"], 0);
    assert!(
        rendered["message"]
            .as_str()
            .is_some_and(|message| message.contains("No matching MCP tools found")),
        "{rendered}"
    );
}

/// `run()` defaults an omitted limit to Top-N and clamps extremes, so one
/// call cannot reintroduce the dump-every-schema behaviour lazy loading
/// exists to avoid.
#[test]
fn tool_search_run_defaults_and_clamps_limit() {
    let mut manager = McpManager::default();
    for index in 0..7 {
        manager.insert_test_tool("misc", &format!("helper_{index}"));
    }

    let rendered: Value =
        serde_json::from_str(&tool_search::run(&manager, r#"{"query":"misc"}"#).unwrap())
            .expect("valid JSON");
    assert_eq!(
        rendered["count"].as_u64(),
        Some(tool_search::DEFAULT_LIMIT as u64)
    );

    for (arguments, expected) in [
        (r#"{"query":"misc","limit":2}"#, 2),
        (r#"{"query":"misc","limit":0}"#, 1),
        (r#"{"query":"misc","limit":100}"#, 7),
    ] {
        let rendered: Value = serde_json::from_str(&tool_search::run(&manager, arguments).unwrap())
            .expect("valid JSON");
        assert_eq!(rendered["count"].as_u64(), Some(expected), "{arguments}");
    }
}

/// A query with no usable tokens after normalisation is a plain no-match,
/// not an error: there is nothing to rank.
#[test]
fn tool_search_treats_untokenizable_queries_as_no_results() {
    let manager = search_bench_manager();

    for arguments in [r#"{"query":"___"}"#, r#"{"query":"a"}"#] {
        let rendered: Value = serde_json::from_str(&tool_search::run(&manager, arguments).unwrap())
            .expect("valid JSON");
        assert_eq!(rendered["count"], 0, "{arguments}");
        assert!(
            rendered["message"]
                .as_str()
                .is_some_and(|message| message.contains("No matching MCP tools found")),
            "{rendered}"
        );
    }
}

#[test]
fn tool_search_rejects_bad_arguments() {
    let manager = search_bench_manager();

    assert!(tool_search::run(&manager, "{}").is_err());
    assert!(tool_search::run(&manager, r#"{"query":"   "}"#).is_err());
    assert!(tool_search::run(&manager, "not json").is_err());
}

/// End to end through dispatch: an agent task's `tool_search` call returns
/// the compact ranking, gated like every task tool behind a task.
#[test]
fn tool_search_runs_through_dispatch_for_an_agent_task() {
    let dir = tempdir().unwrap();
    let mut proxy = NoopProxy {
        agent_runtime: Some(crate::test_support::test_runtime(dir.path())),
        ..NoopProxy::default()
    };
    let mcp = Arc::new(RwLock::new(search_bench_manager()));
    let tool_call = json!({
        "function": {"name": "tool_search", "arguments": "{\"query\":\"search github issues\"}"}
    });

    let result = execute_tool_call(&tool_call, &mcp, &HookContext::disabled(), &mut proxy).unwrap();

    assert_eq!(result.outcome, ToolOutcome::Success);
    let rendered: Value = serde_json::from_str(&result.content).expect("valid JSON");
    assert_eq!(rendered["results"][0]["name"], "mcp__github__search_issues");
}
