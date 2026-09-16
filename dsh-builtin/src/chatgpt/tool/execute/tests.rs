use super::*;
use std::sync::{LazyLock, Mutex};
use tempfile::tempdir;

/// Serializes every test that touches `EXECUTE_TOOL_ENV_ALLOWLIST`: the env
/// var is process-global and overrides the proxy's allowlist, so a test that
/// sets it would otherwise decide what an unrelated concurrent test may run.
pub(crate) static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Take the env lock, ignoring poisoning.
///
/// The guard protects a process-global environment variable, not an
/// invariant, so a panicking test leaves nothing broken behind - but
/// `unwrap()` on a poisoned mutex turned one real failure into a screenful
/// of `PoisonError` that hid it.
pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

use crate::test_support::TestShellProxy;
type TestProxy = TestShellProxy;

/// The shell variable is what `(vset ...)` and a plain `FOO=bar` write, so
/// it has to be consulted before the process environment.
/// The flag table only sees `sh -c '…'`. A shell reading stdin carries no
/// flag, and `eval` is not an interpreter invocation at all - both left the
/// guard classifying `printf` or `eval` while `sh` ran something else.
#[test]
fn code_the_guard_cannot_read_is_refused() {
    let args = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    assert!(hidden_code_source("eval", &args(&["rm -rf /"])).is_some());
    assert!(hidden_code_source("sh", &[]).is_some());
    // Built rather than written as a literal so the portability lint does
    // not read it as a claim about where the binary lives.
    assert!(hidden_code_source(&format!("{}/bash", "/bin"), &args(&["-s"])).is_some());

    // A script operand is a file the tools can read; not this rule's business.
    assert!(hidden_code_source("sh", &args(&["build.sh"])).is_none());
    assert!(hidden_code_source("python3", &args(&["main.py"])).is_none());
    // `sudo` is a wrapper; the stage for what it wraps is judged separately.
    assert!(hidden_code_source("sudo", &args(&["-u", "nobody"])).is_none());
    assert!(hidden_code_source("cargo", &args(&["test"])).is_none());
}

/// `bash < script.sh` is the stdin hole written with a redirection.
///
/// `string_eval_flag` returns nothing (there is no `-c`), and the
/// "every argument is a flag" rule does not hold either, so the line used
/// to reach `sh -c` with the guard having classified `bash` alone.
#[test]
fn a_redirected_script_is_refused() {
    let args = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    assert!(hidden_code_source("bash", &args(&["<", "evil.sh"])).is_some());
    assert!(hidden_code_source("sh", &args(&["<evil.sh"])).is_some());
    assert!(hidden_code_source("bash", &args(&["0<", "evil.sh"])).is_some());
    assert!(hidden_code_source("python3", &args(&["<", "evil.py"])).is_some());

    // Input redirection into something that is not an interpreter is
    // ordinary work.
    assert!(hidden_code_source("sort", &args(&["<", "names.txt"])).is_none());
}

#[test]
fn a_redirected_script_is_refused_end_to_end() {
    let mut proxy = TestProxy::default();
    let err = run("{\"command\":\"bash < evil.sh\"}", &mut proxy)
        .expect_err("a redirected script must not run");
    assert!(err.contains("redirected file"), "{err}");
}

/// A backslash is literal inside single quotes, so treating it as an escape
/// swallowed the closing quote and hid the redirection behind it.
#[test]
fn a_backslash_in_single_quotes_does_not_hide_a_redirection() {
    assert!(writes_by_redirection(r"echo 'a' > out"));
    assert!(writes_by_redirection("echo hi > out"));
    assert!(writes_by_redirection("echo hi >> out"));
    // Still not a file write.
    assert!(!writes_by_redirection("make 2>&1"));
    assert!(!writes_by_redirection("echo 'a > b'"));
}

#[test]
fn load_allowlist_prefers_the_shell_variable_over_the_process_environment() {
    let _lock = env_lock();
    let _guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "from-process-env");
    assert_eq!(
        load_allowed_commands(vec![], Some("from-shell-var".to_string())).unwrap(),
        vec!["from-shell-var"]
    );
}

#[test]
fn load_allowlist_prefers_env() {
    let _lock = env_lock();
    let _guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls,git\ncat");
    assert_eq!(
        load_allowed_commands(vec![], None).unwrap(),
        vec!["cat", "git", "ls"]
    );
}

#[test]
fn load_allowlist_reads_config_file() {
    let _lock = env_lock();
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("allow.json");
    let contents = json!({ "allowed_commands": ["cargo"] }).to_string();
    std::fs::write(&config_path, contents).unwrap();

    let _env_guard = EnvGuard::set(
        EXECUTE_TOOL_CONFIG_OVERRIDE_ENV,
        config_path.to_str().unwrap(),
    );
    let _allow_env = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");
    assert_eq!(load_allowed_commands(vec![], None).unwrap(), vec!["cargo"]);
}

#[test]
fn load_allowlist_merges_runtime_entries() {
    let _lock = env_lock();
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("allow.json");
    let contents = json!({ "allowed_commands": ["cargo"] }).to_string();
    std::fs::write(&config_path, contents).unwrap();

    let _env_guard = EnvGuard::set(
        EXECUTE_TOOL_CONFIG_OVERRIDE_ENV,
        config_path.to_str().unwrap(),
    );
    let _allow_env = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");

    let allowlist =
        load_allowed_commands(vec!["ls".to_string(), "cargo".to_string()], None).unwrap();
    assert_eq!(allowlist, vec!["cargo", "ls"]);
}

/// A command the policy denies is refused, and the refusal names it.
#[test]
fn run_reports_the_command_the_policy_denied() {
    let _lock = env_lock();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls");
    let mut proxy = TestProxy {
        execute_allowlist: vec!["ls".to_string()],
        current_dir: std::env::current_dir().unwrap(),
        agent_verdict: AgentCommandVerdict::Denied("not on this host".to_string()),
        confirm_result: true,
        ..TestProxy::default()
    };
    let result = run("{\"command\":\"cat README.md\"}", &mut proxy);
    let err = result.expect_err("command should be rejected");
    assert!(err.contains("`cat README.md`"), "{err}");
    assert!(err.contains("not on this host"), "{err}");
}

/// Not being on the allowlist is a question now, not a refusal.
///
/// The allowlist ships empty, so the old "allowlisted or rejected" rule
/// meant a fresh install could not run a single command.
#[test]
fn run_asks_rather_than_refusing_a_command_outside_the_allowlist() {
    let _lock = env_lock();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");
    let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut proxy = TestProxy {
        execute_allowlist: vec![],
        current_dir: std::env::current_dir().unwrap(),
        agent_verdict: AgentCommandVerdict::Confirm("unknown command".to_string()),
        confirm_counter: Some(confirm_calls.clone()),
        confirm_result: false,
        ..TestProxy::default()
    };

    let result = run("{\"command\":\"cat README.md\"}", &mut proxy).unwrap();

    assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(result.contains("cancelled by user"), "{result}");
}

/// "Always" is what keeps a long run from becoming a prompt per step.
#[test]
fn run_remembers_an_always_approval_for_the_session() {
    let _lock = env_lock();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");
    let mut proxy = TestProxy {
        current_dir: std::env::current_dir().unwrap(),
        agent_verdict: AgentCommandVerdict::Confirm("unknown command".to_string()),
        approval_decision: Some(ApprovalDecision::AllowAlways),
        ..TestProxy::default()
    };

    run("{\"command\":\"true\"}", &mut proxy).unwrap();

    assert_eq!(proxy.agent_session_allowlist, vec!["true".to_string()]);
}

/// Pipelines run now. Refusing them outright meant `cargo test | tail`
/// could not be expressed, so every multi-step job cost a round trip per
/// step; what makes it safe is the policy verdict, not a token blacklist.
#[test]
fn run_executes_a_pipeline_the_policy_allows() {
    let _lock = env_lock();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");
    let mut proxy = TestProxy {
        execute_allowlist: vec![],
        current_dir: std::env::current_dir().unwrap(),
        agent_verdict: AgentCommandVerdict::Allowed,
        confirm_result: false,
        ..TestProxy::default()
    };

    let result = run("{\"command\":\"printf 'a\\nb\\n' | tail -1\"}", &mut proxy).unwrap();
    let parsed: Value = serde_json::from_str(&result).unwrap();

    assert_eq!(parsed["exit_code"], 0);
    assert_eq!(parsed["stdout"].as_str().unwrap().trim(), "b");
}

/// The shell's parser *runs* `$(...)` while building its job list, so a
/// line carrying one would have executed during the safety check - before
/// the user was asked, and again for real afterwards.
#[test]
fn substitution_is_refused_before_anything_parses_the_line() {
    let _lock = env_lock();
    let mut proxy = TestProxy {
        current_dir: std::env::current_dir().unwrap(),
        agent_verdict: AgentCommandVerdict::Allowed,
        ..TestProxy::default()
    };

    for command in [
        "echo $(whoami)",
        "echo `whoami`",
        "diff <(ls) <(ls)",
        "(cd /tmp && ls)",
    ] {
        let arguments = json!({ "command": command }).to_string();
        let err = run(&arguments, &mut proxy).expect_err("{command} must not reach the parser");
        assert!(err.contains("does not allow"), "{command}: {err}");
    }
}

/// Single quotes suppress every expansion, so a `$(` inside them is text.
#[test]
fn a_dollar_paren_inside_single_quotes_is_not_a_substitution() {
    assert!(substitution_construct("grep '$(' file").is_none());
    assert!(substitution_construct("echo $(date)").is_some());
}

/// Double quotes do *not* suppress `$(...)`, and an apostrophe inside them
/// is a literal. Treating that apostrophe as an opening quote made the rest
/// of the line look quoted, so this substitution reached the parser - which
/// runs it.
#[test]
fn quoting_rules_match_the_shells() {
    let detected = |command: &str| substitution_construct(command).is_some();

    // Still a substitution despite the quotes around it.
    assert!(detected(r#"echo "it's $(whoami)""#));
    assert!(detected(r#"echo "$(date)""#));
    assert!(detected(r#"echo "it's `date`""#));

    // Genuinely literal.
    assert!(!detected(r#"grep "it's fine" file"#));
    assert!(!detected("echo 'no $(sub) here'"));
    assert!(!detected(r#"echo "a (b) c""#));
    assert!(!detected("printf 'a\nb\n' | tail -1"));

    // `echo \$(date)` escapes the dollar but leaves a bare `(`, which is a
    // subshell. Refusing it is the safe reading: this check runs before the
    // parser, where a false positive costs one rephrasing and a false
    // negative costs an unreviewed execution.
    assert!(detected("echo \\$(date)"));
}

/// A newline is ordinary whitespace to the tokenizer, so a second command
/// hid behind the first: only `ls` was inspected, and a bare `ls` entry
/// then waved the whole line through without a prompt.
#[test]
fn a_newline_starts_a_new_stage() {
    let stages = command_stages("ls\nbash -c 'rm -rf ~'").unwrap();
    let seen: Vec<&str> = stages.iter().map(|stage| stage.program.as_str()).collect();

    assert_eq!(seen, vec!["ls", "bash"]);
}

/// A newline inside quotes is data. Splitting on it left two fragments
/// with unbalanced quotes and failed to parse a legitimate `printf`.
#[test]
fn a_quoted_newline_does_not_start_a_new_stage() {
    let stages = command_stages("printf 'a\nb\n' | tail -1").unwrap();
    let seen: Vec<&str> = stages.iter().map(|stage| stage.program.as_str()).collect();

    assert_eq!(seen, vec!["printf", "tail"]);
}

#[test]
fn a_hidden_second_command_still_reaches_the_string_eval_refusal() {
    let _lock = env_lock();
    let mut proxy = TestProxy {
        execute_allowlist: vec!["ls".to_string()],
        current_dir: std::env::current_dir().unwrap(),
        agent_verdict: AgentCommandVerdict::Allowed,
        ..TestProxy::default()
    };

    let arguments = json!({ "command": "ls\nbash -c 'echo hi'" }).to_string();
    let err = run(&arguments, &mut proxy).expect_err("the bash -c must be seen");

    assert!(err.contains("hands it a string to execute"), "{err}");
}

/// `SafetyGuard` classifies by program name, so `sudo` used to hide the
/// `rm` behind it from every `rm` rule.
#[test]
fn a_wrapper_does_not_hide_the_command_it_runs() {
    let stages = command_stages("sudo rm -rf /tmp/x").unwrap();
    let seen: Vec<&str> = stages.iter().map(|stage| stage.program.as_str()).collect();

    assert!(seen.contains(&"rm"), "{seen:?}");
}

/// A redirection is a file write, and the agent's file writes are confirmed.
#[test]
fn a_redirected_write_is_recognised_but_a_descriptor_merge_is_not() {
    assert!(writes_by_redirection("echo x > ~/.ssh/authorized_keys"));
    assert!(writes_by_redirection("cargo build >> build.log"));
    assert!(!writes_by_redirection("cargo test 2>&1 | tail -5"));
    assert!(!writes_by_redirection("echo 'a > b'"));
}

#[test]
fn a_redirected_write_asks_even_when_the_policy_allows_the_command() {
    let _lock = env_lock();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");
    let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut proxy = TestProxy {
        current_dir: std::env::current_dir().unwrap(),
        agent_verdict: AgentCommandVerdict::Allowed,
        confirm_counter: Some(confirm_calls.clone()),
        confirm_result: false,
        ..TestProxy::default()
    };

    let arguments = json!({ "command": "echo hi > /tmp/dsh-write-check" }).to_string();
    let result = run(&arguments, &mut proxy).unwrap();

    assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(result.contains("cancelled by user"), "{result}");
}

/// Approving one line must not approve a longer one that starts with it.
#[test]
fn a_session_approval_does_not_widen_by_prefix() {
    let _lock = env_lock();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "");
    let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut proxy = TestProxy {
        current_dir: std::env::current_dir().unwrap(),
        agent_verdict: AgentCommandVerdict::Confirm("risky".to_string()),
        agent_session_allowlist: vec!["true one".to_string()],
        confirm_counter: Some(confirm_calls.clone()),
        confirm_result: false,
        ..TestProxy::default()
    };

    // The approved line runs without asking again.
    run(&json!({ "command": "true one" }).to_string(), &mut proxy).unwrap();
    assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 0);

    // One extra argument is a different command, and is asked about.
    let result = run(
        &json!({ "command": "true one two" }).to_string(),
        &mut proxy,
    )
    .unwrap();
    assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(result.contains("cancelled by user"), "{result}");
}

/// Every stage of a pipeline is inspected, not just the first.
#[test]
fn command_stages_split_on_operators_and_skip_assignments() {
    let stages = command_stages("FOO=1 cargo test 2>&1 | tail -5 && echo done").unwrap();
    let seen: Vec<&str> = stages.iter().map(|stage| stage.program.as_str()).collect();
    assert_eq!(seen, vec!["cargo", "tail", "echo"]);
    assert_eq!(stages[0].args, vec!["test".to_string(), "2>&1".to_string()]);
}

/// The boundary the hooks `match` layer stands on.
///
/// `HookMatch.programs` reuses this, and its polarity is inverted: for the
/// allowlist a match grants, for a matcher a match *checks*. A change here
/// that "tightens" the allowlist loosens every hook whose `programs` stops
/// matching, so the semantics are pinned in this crate rather than left to
/// the two call sites to agree about.
#[test]
fn command_names_any_looks_through_wrappers_and_stages() {
    let rm = vec!["rm".to_string()];
    for command in [
        "rm -rf /tmp/x",
        "sudo rm -rf /tmp/x",
        "timeout 5 rm /tmp/x",
        "echo hi | rm -rf /tmp/x",
        "cd /tmp && rm -rf x",
    ] {
        assert!(
            command_names_any(command, &rm),
            "`{command}` should have named rm"
        );
    }
    for command in ["ls -la", "echo rm", "cargo test"] {
        assert!(
            !command_names_any(command, &rm),
            "`{command}` should not have named rm"
        );
    }

    // Word prefix, matching the allowlist form the config already uses.
    let push = vec!["git push".to_string()];
    assert!(command_names_any("git push --force", &push));
    assert!(!command_names_any("git pushx", &push));
    assert!(!command_names_any("git status", &push));

    // A line no one can read must not be a way past a check.
    assert!(command_names_any("echo 'unclosed", &rm));
    assert_eq!(command_tokens("echo 'unclosed"), None);

    assert_eq!(
        command_tokens("cat /etc/hosts | tail -1"),
        Some(vec![
            "cat".to_string(),
            "/etc/hosts".to_string(),
            "tail".to_string(),
            "-1".to_string(),
        ])
    );
}

#[test]
fn run_rejects_string_eval_flags() {
    let _lock = env_lock();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "bash");
    let mut proxy = TestProxy {
        execute_allowlist: vec!["bash".to_string()],
        current_dir: std::env::current_dir().unwrap(),
        confirm_result: true,
        ..TestProxy::default()
    };

    // A combined cluster is the same flag wearing a hat.
    for command in ["bash -lc 'echo hi'", "bash -ic 'echo hi'"] {
        let arguments = format!("{{\"command\":\"{command}\"}}");
        let result = run(&arguments, &mut proxy);
        assert!(result.is_err(), "{command} was allowed through");
        assert!(
            result.unwrap_err().contains("hands it a string to execute"),
            "unexpected refusal for {command}"
        );
    }
}

/// Every spelling of "here is some code" has to be caught, and nothing else.
#[test]
fn string_eval_flags_are_recognised_in_every_spelling() {
    let eval = [
        ("bash", vec!["-c", "echo hi"]),
        ("bash", vec!["-lc", "echo hi"]),
        ("bash", vec!["-ic", "echo hi"]),
        ("sh", vec!["-euc", "echo hi"]),
        ("bash", vec!["-o", "pipefail", "-c", "echo hi"]),
        ("zsh", vec!["-ic", "echo hi"]),
        ("fish", vec!["--command", "echo hi"]),
        ("fish", vec!["-C", "echo hi"]),
        ("python3", vec!["-Ec", "print(1)"]),
        ("python3", vec!["-c", "print(1)"]),
        ("perl", vec!["-E", "say 1"]),
        ("perl", vec!["-lne", "print"]),
        ("ruby", vec!["-e", "puts 1"]),
        ("node", vec!["--eval", "1"]),
        ("node", vec!["-pe", "1"]),
        ("pwsh", vec!["-EncodedCommand", "AAA"]),
        ("powershell", vec!["-Comm", "dir"]),
    ];
    for (program, args) in eval {
        let args: Vec<String> = args.into_iter().map(str::to_string).collect();
        assert!(
            string_eval_flag(program, &args).is_some(),
            "{program} {args:?} should have been recognised"
        );
    }

    let plain = [
        // A script is not a string, even when its own arguments look like
        // eval flags.
        ("bash", vec!["script.sh"]),
        ("bash", vec!["script.sh", "-c"]),
        ("bash", vec!["--", "-c"]),
        // Options whose value merely contains an eval letter.
        ("python3", vec!["-Wonce", "script.py"]),
        ("python3", vec!["-m", "http.server"]),
        ("perl", vec!["-Mencoding", "script.pl"]),
        ("ruby", vec!["-E", "utf-8", "script.rb"]),
        ("node", vec!["server.js"]),
        ("node", vec!["-r", "esm", "server.js"]),
        // Not an interpreter at all.
        ("git", vec!["-c", "user.name=x", "status"]),
        ("ls", vec!["-c"]),
    ];
    for (program, args) in plain {
        let args: Vec<String> = args.into_iter().map(str::to_string).collect();
        assert_eq!(
            string_eval_flag(program, &args),
            None,
            "{program} {args:?} should have been left alone"
        );
    }
}

#[test]
fn basename_allowlist_does_not_match_path_qualified_program() {
    assert!(command_is_allowlisted(
        "git",
        &["status".to_string()],
        &["git".to_string()]
    ));
    assert!(!command_is_allowlisted(
        "/tmp/git",
        &["status".to_string()],
        &["git".to_string()]
    ));
    assert!(command_is_allowlisted(
        "/tmp/git",
        &["status".to_string()],
        &["/tmp/git".to_string()]
    ));
}

/// A `git` entry must not silently authorise `/tmp/git`.
///
/// It is no longer a flat refusal - the policy decides - but it must still
/// not take the allowlist fast path, so the user is asked.
#[test]
fn run_does_not_let_a_path_qualified_program_ride_a_basename_allowlist() {
    let _lock = env_lock();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "git");
    let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut proxy = TestProxy {
        execute_allowlist: vec!["git".to_string()],
        current_dir: std::env::current_dir().unwrap(),
        agent_verdict: AgentCommandVerdict::Confirm("unknown command".to_string()),
        confirm_counter: Some(confirm_calls.clone()),
        confirm_result: false,
        ..TestProxy::default()
    };

    let result = run("{\"command\":\"/tmp/git status\"}", &mut proxy).unwrap();

    assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(result.contains("cancelled by user"), "{result}");
}

/// A project skill arrives with a `git clone` and the prompt points the
/// model straight at it. Judging only the personal skills root let a script
/// under `<project>/.dogesh/skills` fall through to the ordinary command
/// policy and run unasked wherever that policy said yes.
#[test]
fn a_script_under_the_project_skills_directory_always_asks() {
    let _lock = env_lock();
    let config_root = tempdir().unwrap();
    let _cfg_guard = EnvGuard::set("XDG_CONFIG_HOME", config_root.path().to_str().unwrap());

    let project = tempdir().unwrap();
    let project_dir = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(project_dir.join(".git")).unwrap();
    let skills_dir = project_dir.join(".dogesh/skills/deploy/scripts");
    std::fs::create_dir_all(&skills_dir).unwrap();
    let script_path = skills_dir.join("run.sh");
    std::fs::write(&script_path, "#!/usr/bin/env bash\necho hello\n").unwrap();

    let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut proxy = TestProxy {
        // The policy would wave this through; the skill-script rule must
        // still win.
        agent_verdict: AgentCommandVerdict::Allowed,
        current_dir: project_dir,
        confirm_counter: Some(confirm_calls.clone()),
        confirm_result: false,
        ..TestProxy::default()
    };

    let command = format!("{{\"command\":\"{}\"}}", script_path.to_string_lossy());
    let result = run(&command, &mut proxy);

    assert_eq!(result.unwrap(), "Execution cancelled by user.".to_string());
    assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// The interop root arrives with the same `git clone`, so it is held to the
/// same rule. This follows `skill_roots`, and the test is what keeps it
/// following: a third root added without one would be silently exempt.
#[test]
fn a_script_under_the_project_agents_skills_directory_always_asks() {
    let _lock = env_lock();
    let config_root = tempdir().unwrap();
    let _cfg_guard = EnvGuard::set("XDG_CONFIG_HOME", config_root.path().to_str().unwrap());

    let project = tempdir().unwrap();
    let project_dir = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(project_dir.join(".git")).unwrap();
    let skills_dir = project_dir.join(".agents/skills/deploy/scripts");
    std::fs::create_dir_all(&skills_dir).unwrap();
    let script_path = skills_dir.join("run.sh");
    std::fs::write(&script_path, "#!/usr/bin/env bash\necho hello\n").unwrap();

    let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut proxy = TestProxy {
        agent_verdict: AgentCommandVerdict::Allowed,
        current_dir: project_dir,
        confirm_counter: Some(confirm_calls.clone()),
        confirm_result: false,
        ..TestProxy::default()
    };

    // Through an interpreter, so this also covers the "every token of every
    // stage" rule for the new root rather than only its program position.
    let command = format!("{{\"command\":\"bash {}\"}}", script_path.to_string_lossy());
    let result = run(&command, &mut proxy);

    assert_eq!(result.unwrap(), "Execution cancelled by user.".to_string());
    assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// `--allow-command` names a line the person read. A skill script is a file
/// the agent can also write, and one a `git clone` can deliver, so the grant
/// must not cover it. The rule was stated in a comment and enforced only on
/// the interactive path.
#[test]
fn a_skill_script_is_not_covered_by_an_agent_grant() {
    let _lock = env_lock();
    let config_root = tempdir().unwrap();
    let _cfg_guard = EnvGuard::set("XDG_CONFIG_HOME", config_root.path().to_str().unwrap());

    let project = tempdir().unwrap();
    let project_dir = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(project_dir.join(".git")).unwrap();
    let skills_dir = project_dir.join(".dogesh/skills/deploy/scripts");
    std::fs::create_dir_all(&skills_dir).unwrap();
    let script_path = skills_dir.join("run.sh");
    std::fs::write(&script_path, "#!/usr/bin/env bash\necho hello\n").unwrap();

    let mut proxy = TestProxy {
        // The grant says yes to everything; the skill-script rule still wins.
        agent_verdict: AgentCommandVerdict::Allowed,
        agent_runtime: Some(crate::test_support::test_runtime(&project_dir)),
        current_dir: project_dir,
        ..TestProxy::default()
    };

    let command = format!("{{\"command\":\"{}\"}}", script_path.to_string_lossy());
    let err = run(&command, &mut proxy).expect_err("a task must stop for approval");

    assert!(err.contains("skill script permission required"), "{err}");
}

/// `bash <skill>/run.sh` used to walk straight past the skill-script rule:
/// `bash` is not a transparent wrapper, so the stage stayed
/// `("bash", ["…run.sh"])` and only the program was judged.
#[test]
fn a_skill_script_run_through_an_interpreter_still_asks() {
    let _lock = env_lock();
    let config_root = tempdir().unwrap();
    let _cfg_guard = EnvGuard::set("XDG_CONFIG_HOME", config_root.path().to_str().unwrap());

    let project = tempdir().unwrap();
    let project_dir = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(project_dir.join(".git")).unwrap();
    let skills_dir = project_dir.join(".dogesh/skills/deploy/scripts");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(skills_dir.join("run.sh"), "echo hello\n").unwrap();

    for command in [
        "bash .dogesh/skills/deploy/scripts/run.sh",
        "python3 .dogesh/skills/deploy/scripts/run.sh",
        "sh ./.dogesh/skills/deploy/scripts/run.sh",
    ] {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut proxy = TestProxy {
            agent_verdict: AgentCommandVerdict::Allowed,
            current_dir: project_dir.clone(),
            confirm_counter: Some(calls.clone()),
            confirm_result: false,
            ..TestProxy::default()
        };

        let result = run(&format!("{{\"command\":\"{command}\"}}"), &mut proxy).unwrap();

        assert_eq!(result, "Execution cancelled by user.", "{command}");
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "{command}"
        );
    }
}

/// `execute` takes a `cwd`, and the rule resolved relative tokens against
/// the shell's directory instead, so pointing `cwd` at the skill directory
/// made `./run.sh` look like an ordinary local script.
#[test]
fn a_skill_script_reached_through_the_cwd_argument_still_asks() {
    let _lock = env_lock();
    let config_root = tempdir().unwrap();
    let _cfg_guard = EnvGuard::set("XDG_CONFIG_HOME", config_root.path().to_str().unwrap());

    let project = tempdir().unwrap();
    let project_dir = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(project_dir.join(".git")).unwrap();
    let skills_dir = project_dir.join(".dogesh/skills/deploy/scripts");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(skills_dir.join("run.sh"), "echo hello\n").unwrap();

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut proxy = TestProxy {
        agent_verdict: AgentCommandVerdict::Allowed,
        current_dir: project_dir,
        confirm_counter: Some(calls.clone()),
        confirm_result: false,
        ..TestProxy::default()
    };

    let result = run(
        r#"{"command":"./run.sh","cwd":".dogesh/skills/deploy/scripts"}"#,
        &mut proxy,
    )
    .unwrap();

    assert_eq!(result, "Execution cancelled by user.");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// The rule must not fire on every command that happens to name a path.
#[test]
fn an_ordinary_path_argument_does_not_trigger_the_skill_script_rule() {
    let _lock = env_lock();
    let config_root = tempdir().unwrap();
    let _cfg_guard = EnvGuard::set("XDG_CONFIG_HOME", config_root.path().to_str().unwrap());

    let project = tempdir().unwrap();
    let project_dir = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(project_dir.join(".git")).unwrap();
    std::fs::write(project_dir.join("notes.txt"), "hi\n").unwrap();

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut proxy = TestProxy {
        agent_verdict: AgentCommandVerdict::Allowed,
        current_dir: project_dir,
        confirm_counter: Some(calls.clone()),
        confirm_result: true,
        ..TestProxy::default()
    };

    run(r#"{"command":"cat ./notes.txt"}"#, &mut proxy).unwrap();

    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// The same task path still honours an ordinary grant, so the check above is
/// about skill scripts and not about tasks in general.
#[test]
fn an_ordinary_command_still_runs_under_an_agent_grant() {
    let _lock = env_lock();
    let dir = tempdir().unwrap();
    let dir_path = std::fs::canonicalize(dir.path()).unwrap();
    let mut proxy = TestProxy {
        agent_verdict: AgentCommandVerdict::Allowed,
        agent_runtime: Some(crate::test_support::test_runtime(&dir_path)),
        current_dir: dir_path,
        ..TestProxy::default()
    };

    let result = run("{\"command\":\"echo hi\"}", &mut proxy).unwrap();

    assert!(result.contains("hi"), "{result}");
}

#[test]
fn run_skips_confirmation_for_allowlisted_command() {
    let _lock = env_lock();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls");
    let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut proxy = TestProxy {
        execute_allowlist: vec!["ls".to_string()],
        current_dir: std::env::current_dir().unwrap(),
        confirm_counter: Some(confirm_calls.clone()),
        confirm_result: true,
        ..TestProxy::default()
    };
    let result = run("{\"command\":\"ls -la\"}", &mut proxy);
    assert!(result.is_ok());
    assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn run_requires_confirmation_for_skill_script() {
    let _lock = env_lock();
    let config_root = tempdir().unwrap();
    let skills_dir = config_root.path().join("dogesh/skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    let script_path = skills_dir.join("script.sh");
    std::fs::write(&script_path, "#!/usr/bin/env bash\necho hello\n").unwrap();

    let _cfg_guard = EnvGuard::set("XDG_CONFIG_HOME", config_root.path().to_str().unwrap());

    let confirm_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut proxy = TestProxy {
        execute_allowlist: vec![],
        current_dir: std::env::current_dir().unwrap(),
        confirm_counter: Some(confirm_calls.clone()),
        confirm_result: false,
        ..TestProxy::default()
    };

    let command = format!("{{\"command\":\"{}\"}}", script_path.to_string_lossy());
    let result = run(&command, &mut proxy);

    assert_eq!(result.unwrap(), "Execution cancelled by user.".to_string());
    assert_eq!(confirm_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn run_kills_a_command_that_exceeds_its_timeout() {
    let _lock = env_lock();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "sleep");
    let mut proxy = TestProxy {
        execute_allowlist: vec!["sleep".to_string()],
        current_dir: std::env::current_dir().unwrap(),
        confirm_result: true,
        ..TestProxy::default()
    };

    let started = Instant::now();
    let result = run("{\"command\":\"sleep 30\",\"timeout_ms\":1000}", &mut proxy).unwrap();
    let elapsed = started.elapsed();

    assert!(result.contains("exceeded timeout_ms=1000"), "{result}");
    assert!(result.contains("\"exit_code\":-1"), "{result}");
    assert!(
        elapsed < Duration::from_secs(20),
        "did not return early: {elapsed:?}"
    );
}

#[test]
fn run_returns_stderr_alongside_exit_code() {
    let _lock = env_lock();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls");
    let mut proxy = TestProxy {
        execute_allowlist: vec!["ls".to_string()],
        current_dir: std::env::current_dir().unwrap(),
        confirm_result: true,
        ..TestProxy::default()
    };

    let result = run(
        "{\"command\":\"ls /definitely-missing-path-for-dsh-test\"}",
        &mut proxy,
    )
    .unwrap();
    let parsed: Value = serde_json::from_str(&result).unwrap();

    assert_ne!(parsed["exit_code"].as_i64().unwrap(), 0);
    assert!(
        !parsed["stderr"].as_str().unwrap().is_empty(),
        "stderr was dropped: {result}"
    );
}

#[test]
fn render_result_stays_parseable_when_escaping_inflates_it() {
    // Regression: the per-stream caps bound raw text, but JSON escaping can
    // multiply it, and the shared truncator then cut the middle out of the
    // serialized object.
    let noisy = "\u{1b}[31mboom\n".repeat(2000);
    let rendered = render_result(1, &noisy, &noisy, None);

    assert!(
        rendered.len() <= crate::chatgpt::tool::MAX_OUTPUT_LENGTH,
        "result is {} bytes",
        rendered.len()
    );
    let parsed: Value = serde_json::from_str(&rendered).expect("result must be valid JSON");
    assert_eq!(parsed["exit_code"], 1);
    assert!(!parsed["stdout"].as_str().unwrap().is_empty());
    assert!(!parsed["stderr"].as_str().unwrap().is_empty());
}

#[test]
fn render_result_reports_a_timeout_note() {
    let rendered = render_result(-1, "out", "", Some("killed".to_string()));
    let parsed: Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(parsed["note"], "killed");
}

#[test]
fn run_returns_even_while_a_grandchild_holds_the_pipe() {
    // A script that backgrounds a process keeps the write end of stdout
    // open after it exits, so waiting for EOF would hang the shell.
    let _lock = env_lock();

    let dir = tempdir().unwrap();
    let script = dir.path().join("spawner.sh");
    std::fs::write(&script, "#!/bin/sh\nsleep 5 &\necho started\n").unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&script, permissions).unwrap();

    let allowlist = script.to_string_lossy().to_string();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, &allowlist);
    let mut proxy = TestProxy {
        execute_allowlist: vec![allowlist.clone()],
        current_dir: std::env::current_dir().unwrap(),
        confirm_result: true,
        ..TestProxy::default()
    };

    let started = Instant::now();
    let result = run(&format!("{{\"command\":\"{allowlist}\"}}"), &mut proxy).unwrap();
    let elapsed = started.elapsed();

    let parsed: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(parsed["exit_code"].as_i64().unwrap(), 0);
    assert!(
        elapsed < DRAIN_GRACE + Duration::from_secs(1),
        "waited for the grandchild: {elapsed:?}"
    );
    // Returning is not enough: the script's own output has to survive the
    // grace period. Waiting for a single end-of-stream message meant giving
    // up with an empty stdout even though the line had already been read.
    assert!(
        parsed["stdout"].as_str().unwrap().contains("started"),
        "the script's output was dropped: {result}"
    );
}

/// The end of a long output is where the compiler error is, so a capture
/// that overflows its cap has to keep the tail, not just the head.
#[test]
fn a_capture_over_the_cap_keeps_the_tail() {
    let mut capture = CappedCapture::default();
    capture.push(b"HEAD-MARKER");
    capture.push(&vec![b'x'; MAX_CAPTURED_BYTES * 2]);
    capture.push(b"TAIL-MARKER");

    let snapshot = String::from_utf8_lossy(&capture.snapshot()).into_owned();
    assert!(snapshot.starts_with("HEAD-MARKER"), "lost the head");
    assert!(snapshot.ends_with("TAIL-MARKER"), "lost the tail");
    assert!(
        snapshot.contains("dropped"),
        "the omission is not reported: {}",
        &snapshot[..80.min(snapshot.len())]
    );
}

/// Under the cap nothing is rewritten, marker included.
#[test]
fn a_capture_under_the_cap_is_verbatim() {
    let mut capture = CappedCapture::default();
    capture.push(b"one\n");
    capture.push(b"two\n");

    assert_eq!(capture.snapshot(), b"one\ntwo\n");
}

#[test]
fn output_written_before_a_grandchild_outlives_the_command_is_kept() {
    // Same shape as above with more to lose: every line the script itself
    // printed must come back, not just the first.
    let _lock = env_lock();

    let dir = tempdir().unwrap();
    let script = dir.path().join("chatty-spawner.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nsleep 5 &\ni=0\nwhile [ $i -lt 200 ]; do echo \"line-$i\"; i=$((i+1)); done\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&script, permissions).unwrap();

    let allowlist = script.to_string_lossy().to_string();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, &allowlist);
    let mut proxy = TestProxy {
        execute_allowlist: vec![allowlist.clone()],
        current_dir: std::env::current_dir().unwrap(),
        confirm_result: true,
        ..TestProxy::default()
    };

    let result = run(&format!("{{\"command\":\"{allowlist}\"}}"), &mut proxy).unwrap();
    let parsed: Value = serde_json::from_str(&result).unwrap();
    let stdout = parsed["stdout"].as_str().unwrap();

    assert!(stdout.contains("line-0"), "lost the head: {result}");
    assert!(stdout.contains("line-199"), "lost the tail: {result}");
}

#[test]
fn a_large_stdout_does_not_evict_stderr() {
    // Regression: the whole result JSON used to be cut from the front, so a
    // chatty stdout dropped the error message the model needed.
    let _lock = env_lock();
    let _env_guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls");

    let dir = tempdir().unwrap();
    for index in 0..300 {
        std::fs::write(dir.path().join(format!("file-{index:0>40}")), b"x").unwrap();
    }

    let mut proxy = TestProxy {
        execute_allowlist: vec!["ls".to_string()],
        current_dir: std::env::current_dir().unwrap(),
        confirm_result: true,
        ..TestProxy::default()
    };

    let command = format!(
        "{{\"command\":\"ls {} /definitely-missing-path-for-dsh-test\"}}",
        dir.path().display()
    );
    let result = run(&command, &mut proxy).unwrap();
    let parsed: Value = serde_json::from_str(&result).unwrap();

    let stdout = parsed["stdout"].as_str().unwrap();
    let stderr = parsed["stderr"].as_str().unwrap();

    assert!(stdout.len() > MAX_STREAM_CHARS / 2, "stdout was empty");
    assert!(stdout.contains("truncated"), "stdout was not truncated");
    assert!(!stderr.is_empty(), "stderr was dropped: {result}");
    assert_ne!(parsed["exit_code"].as_i64().unwrap(), 0);
}

pub(crate) struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var(key).ok();
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.previous {
            unsafe {
                env::set_var(self.key, value);
            }
        } else {
            unsafe {
                env::remove_var(self.key);
            }
        }
    }
}
