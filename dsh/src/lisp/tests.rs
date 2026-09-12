use super::*;
use std::ffi::OsString;
use std::time::{SystemTime, UNIX_EPOCH};

fn init() {
    let _ = tracing_subscriber::fmt::try_init();
}

fn with_test_config_home<F>(test_fn: F)
where
    F: FnOnce(),
{
    let _guard = crate::test_env_lock();
    let previous = std::env::var_os("XDG_CONFIG_HOME");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsh-test-config-{unique}"));
    std::fs::create_dir_all(&dir).unwrap();

    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", OsString::from(dir));
    }

    test_fn();

    match previous {
        Some(value) => unsafe {
            std::env::set_var("XDG_CONFIG_HOME", value);
        },
        None => unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        },
    }
}

#[test]
fn test_run_lisp() {
    init();
    let env = Environment::new();
    let engine = LispEngine::new(env);
    let _res = engine.borrow().run("(alias \"e\" \"emacs\")");
}

#[test]
fn command_ledger_preference_is_off_by_default_and_configurable() {
    init();
    let env = Environment::new();
    let engine = LispEngine::new(env.clone());
    assert_eq!(
        env.read().variable_state.command_ledger_mode,
        crate::history::CommandLedgerMode::Off
    );
    engine
        .borrow()
        .run("(pref-command-ledger \"metadata\")")
        .unwrap();
    assert_eq!(
        env.read().variable_state.command_ledger_mode,
        crate::history::CommandLedgerMode::Metadata
    );
    assert!(
        engine
            .borrow()
            .run("(pref-command-ledger \"invalid\")")
            .is_err()
    );
}

#[test]
fn lisp_can_define_command_scoped_abbreviation() {
    init();
    let env = Environment::new();
    let engine = LispEngine::new(env.clone());
    engine
        .borrow()
        .run("(abbr-command \"git\" \"co\" \"checkout\")")
        .unwrap();
    assert_eq!(
        env.read()
            .variable_state
            .command_abbreviations
            .get("git")
            .and_then(|entries| entries.get("co"))
            .map(String::as_str),
        Some("checkout")
    );
}

#[test]
fn test_apply_fn() {
    init();
    let env = Environment::new();
    let engine = LispEngine::new(env);
    let res = engine.borrow().run(
        "
(begin
  (defun log (str)
    (print str)))",
    );
    assert!(res.is_ok());

    let func = engine.borrow().run("log").unwrap();
    let args = vec![Value::String("abcdefg".to_owned())];
    let res = func.apply(engine.borrow().env.clone(), args);
    assert!(res.is_ok());
}

#[tokio::test]
#[ignore = "requires shell execution context"]
async fn test_call_fn() {
    init();
    let env = Environment::new();
    let engine = LispEngine::new(env);
    let res = engine.borrow().run(
        "
(begin
  (defun log (str)
    (print str))
  (defun adder (x y)
    (+ x y))
  (defun call ()
    (sh \"ls -al\"))
)
",
    );
    assert!(res.is_ok());

    let args = vec!["abcdefg".to_string()];
    let res = engine.borrow().run_func("log", args);
    assert!(res.is_ok());

    let args = vec![Value::Int(IntType::from(1)), Value::Int(IntType::from(2))];
    let res = engine.borrow().run_func_values("adder", args);
    assert!(res.is_ok());
    println!("{res:?}");

    let args = vec![];
    let res = engine.borrow().run_func_values("call", args);
    assert!(res.is_ok());
    println!("{res:?}");
}

#[test]
fn test_register_action() {
    init();
    let env = Environment::new();
    let engine = LispEngine::new(env);

    // 1. Defun a function
    let _ = engine
        .borrow()
        .run("(defun my-test-func () (print \"success\"))");

    // 2. Register it as an action
    let res = engine
        .borrow()
        .run("(register-action \"My Test Action\" \"A test action\" \"my-test-func\")");
    assert!(res.is_ok());

    // 3. Verify it's in the registry
    let registry = crate::command_palette::REGISTRY.read();
    let actions = registry.get_all();
    let action = actions
        .iter()
        .find(|a| a.name() == "My Test Action")
        .expect("Action not found in registry");
    assert_eq!(action.description(), "A test action");

    // 4. (Optional) Check if it works without real Shell if possible,
    // but since execute() needs &mut Shell, we'll stop here for unit test
    // or just verify it doesn't panic when we look it up.
}

#[test]
fn bind_and_unbind_update_the_environment() {
    init();
    let env = Environment::new();
    let engine = LispEngine::new(env.clone());

    assert!(
        engine
            .borrow()
            .run("(bind \"ctrl-g\" \"cancel-completion\")")
            .is_ok()
    );
    assert!(
        env.read()
            .key_binding_descriptions()
            .contains(&"ctrl-g -> cancel-completion".to_string())
    );

    // An unknown action name is taken as a Lisp function, not an error:
    // the function may be defined later in config.lisp.
    assert!(engine.borrow().run("(bind \"ctrl-t\" \"my-fn\")").is_ok());
    assert!(
        env.read()
            .key_binding_descriptions()
            .contains(&"ctrl-t -> lisp:my-fn".to_string())
    );

    assert!(engine.borrow().run("(unbind \"ctrl-g\")").is_ok());
    assert!(
        !env.read()
            .key_binding_descriptions()
            .iter()
            .any(|line| line.starts_with("ctrl-g "))
    );
}

#[test]
fn sched_add_registers_a_task_from_lisp() {
    init();
    let env = Environment::new();
    let engine = LispEngine::new(env.clone());

    let res = engine
        .borrow()
        .run("(sched-add \"fetch\" \"5m\" \"git fetch --all\" \"change\")");
    assert!(res.is_ok(), "{res:?}");

    let listed = env.read().sched_descriptions();
    assert_eq!(
        listed,
        vec!["fetch every 5m -> git fetch --all".to_string()]
    );

    assert!(engine.borrow().run("(sched-pause \"fetch\")").is_ok());
    assert!(env.read().sched_descriptions()[0].ends_with("(paused)"));

    assert!(engine.borrow().run("(sched-remove \"fetch\")").is_ok());
    assert!(env.read().sched_descriptions().is_empty());
}

#[test]
fn sched_add_rejects_bad_arguments() {
    init();
    let env = Environment::new();
    let engine = LispEngine::new(env);

    // Interval below the 5s floor.
    assert!(
        engine
            .borrow()
            .run("(sched-add \"a\" \"1s\" \"true\")")
            .is_err()
    );
    assert!(
        engine
            .borrow()
            .run("(sched-add \"a\" \"5x\" \"true\")")
            .is_err()
    );
    assert!(
        engine
            .borrow()
            .run("(sched-add \"a\" \"5m\" \"true\" \"sometimes\")")
            .is_err()
    );
    assert!(engine.borrow().run("(sched-add \"a\" \"5m\")").is_err());
}

#[test]
fn bind_rejects_an_unparseable_key() {
    init();
    let env = Environment::new();
    let engine = LispEngine::new(env);

    assert!(
        engine
            .borrow()
            .run("(bind \"ctrl-nope\" \"undo\")")
            .is_err()
    );
    assert!(engine.borrow().run("(bind \"ctrl-g\")").is_err());
}

/// A failed config must not leave half-applied bindings behind.
#[test]
fn config_rollback_restores_key_bindings() {
    init();
    with_test_config_home(|| {
        let env = Environment::new();
        let engine = LispEngine::new(env.clone());

        let config_path = environment::get_config_file(CONFIG_FILE).unwrap();
        std::fs::write(
            &config_path,
            "(bind \"ctrl-g\" \"cancel-completion\")\n(this-does-not-exist)\n",
        )
        .unwrap();

        assert!(engine.borrow().run_config_lisp().is_err());
        assert!(
            !env.read()
                .key_binding_descriptions()
                .iter()
                .any(|line| line.starts_with("ctrl-g ")),
            "binding survived a failed config load"
        );
    });
}

#[test]
fn run_config_lisp_rolls_back_state_on_error() {
    init();
    with_test_config_home(|| {
        let env = Environment::new();
        let engine = LispEngine::new(env.clone());
        let rollback_action_name = "ROLLBACK_TEST_ACTION_SHOULD_NOT_EXIST";
        let rollback_env_key = "ROLLBACK_TEST_TEMP_ENV";

        let config_path = crate::environment::get_config_file(CONFIG_FILE).unwrap();

        std::fs::write(
            &config_path,
            r#"
(mcp-clear)
(mcp-add-sse "stable" "https://example.com/stable" "stable")
(alias "stable-alias" "echo stable")
(vset "STABLE_VAR" "stable")
(chat-execute-clear)
(chat-execute-add "ls")
(secret-history-mode "redact")
(defun stable-func () "stable")
"#,
        )
        .unwrap();
        engine.borrow().run_config_lisp().unwrap();

        {
            let env_read = env.read();
            assert_eq!(env_read.mcp_servers().len(), 1);
            assert_eq!(env_read.mcp_servers()[0].label, "stable");
            assert!(!env_read.startup_mode);
            assert_eq!(
                env_read.variable_state.alias.get("stable-alias"),
                Some(&"echo stable".to_string())
            );
            assert_eq!(
                env_read.variable_state.variables.get("STABLE_VAR"),
                Some(&"stable".to_string())
            );
            let allowlist = env_read.policy_state.execute_allowlist.read().clone();
            assert_eq!(allowlist, vec!["ls".to_string()]);
            assert_eq!(
                env_read.policy_state.secret_manager.history_mode(),
                crate::secrets::SecretHistoryMode::Redact
            );
        }
        assert!(engine.borrow().has("stable-func"));

        let mcp_runtime_before = {
            let env_read = env.read();
            let manager = env_read.integration_state.mcp_manager.read();
            let mut snapshot = manager.snapshot_runtime_state();
            snapshot
                .session_meta
                .insert("stable".to_string(), std::time::Instant::now());
            snapshot
                .connection_errors
                .insert("stable".to_string(), "seeded".to_string());
            manager.restore_runtime_state(snapshot.clone());
            snapshot
        };

        std::fs::write(
            &config_path,
            format!(
                r#"
(mcp-clear)
(mcp-add-sse "broken" "https://example.com/broken" "broken")
(mcp-disconnect-all)
(alias "broken-alias" "echo broken")
(vset "BROKEN_VAR" "broken")
(chat-execute-clear)
(chat-execute-add "rm -rf /")
(secret-history-mode "none")
(setenv "{rollback_env_key}" "broken")
(register-action "{rollback_action_name}" "Rollback test action" "stable-func")
(defun broken-func () "broken")
(this-function-does-not-exist)
            "#,
            ),
        )
        .unwrap();

        let err = engine.borrow().run_config_lisp().unwrap_err();
        assert!(err.to_string().contains("this-function-does-not-exist"));

        let env_read = env.read();
        assert_eq!(env_read.mcp_servers().len(), 1);
        assert_eq!(env_read.mcp_servers()[0].label, "stable");
        assert!(!env_read.startup_mode);
        assert_eq!(
            env_read.variable_state.alias.get("stable-alias"),
            Some(&"echo stable".to_string())
        );
        assert!(!env_read.variable_state.alias.contains_key("broken-alias"));
        assert_eq!(
            env_read.variable_state.variables.get("STABLE_VAR"),
            Some(&"stable".to_string())
        );
        assert!(!env_read.variable_state.variables.contains_key("BROKEN_VAR"));
        assert_eq!(
            env_read.policy_state.secret_manager.history_mode(),
            crate::secrets::SecretHistoryMode::Redact
        );
        let allowlist = env_read.policy_state.execute_allowlist.read().clone();
        assert_eq!(allowlist, vec!["ls".to_string()]);
        let mcp_runtime_after = env_read
            .integration_state
            .mcp_manager
            .read()
            .snapshot_runtime_state();
        assert_eq!(mcp_runtime_after, mcp_runtime_before);

        assert!(engine.borrow().has("stable-func"));
        assert!(!engine.borrow().has("broken-func"));
        assert!(std::env::var(rollback_env_key).is_err());
        let has_broken_action = crate::command_palette::REGISTRY
            .read()
            .get_all()
            .iter()
            .any(|action| action.name() == rollback_action_name);
        assert!(!has_broken_action);
    });
}
