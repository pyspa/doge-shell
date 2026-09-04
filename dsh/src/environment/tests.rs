//! Tests for the environment module.

use super::*;
use dsh_types::output_history::OutputEntry;
use std::path::Path;

fn init() {
    let _ = tracing_subscriber::fmt::try_init();
}

#[test]
fn environment_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Environment>();
}

#[test]
fn test_lookup() {
    init();
    let env = Environment::new();
    let p = env.read().lookup("touch");
    assert_eq!(Some("/usr/bin/touch".to_string()), p)
}

#[test]
fn test_extend() {
    init();
    let env = Environment::new();
    let env1 = Arc::clone(&env);
    env.write()
        .variable_state
        .variables
        .insert("test".to_string(), "value".to_string());

    let env2 = Environment::extend(env);
    let env2_clone = Arc::clone(&env2);

    env2.write()
        .variable_state
        .variables
        .insert("test2".to_string(), "value2".to_string());

    let env2_clone = env2_clone.read();
    let v = env2_clone.variable_state.variables.get("test");
    assert_eq!("value".to_string(), *v.unwrap());
    assert_eq!(
        "value2".to_string(),
        *env2_clone.variable_state.variables.get("test2").unwrap()
    );

    assert_eq!(2, env1.read().variable_state.variables.len());
}

#[test]
fn extend_copies_shares_and_resets_state_by_group() {
    let parent = Environment::new();
    {
        let mut parent = parent.write();
        parent
            .variable_state
            .alias
            .insert("ll".to_string(), "ls -l".to_string());
        parent.completion_state.input_preferences.auto_pair = true;
        parent
            .completion_state
            .command_cache
            .write()
            .insert("git".to_string(), Some("/usr/bin/git".to_string()));
        *parent.completion_state.executable_names.write() = vec!["git".to_string()];
        parent
            .session_output_state
            .output_history
            .push(OutputEntry::new(
                "echo".to_string(),
                "ok".to_string(),
                String::new(),
                0,
            ));
        parent
            .policy_state
            .secret_manager
            .add_pattern("CUSTOM_[A-Z]+")
            .unwrap();
    }

    let child = Environment::extend(parent.clone());
    let parent_guard = parent.read();
    let child_guard = child.read();

    assert_eq!(
        child_guard.variable_state.alias.get("ll"),
        Some(&"ls -l".to_string())
    );
    assert!(child_guard.completion_state.input_preferences.auto_pair);
    assert!(Arc::ptr_eq(
        &parent_guard.integration_state.mcp_manager,
        &child_guard.integration_state.mcp_manager
    ));
    assert!(Arc::ptr_eq(
        &parent_guard.policy_state.execute_allowlist,
        &child_guard.policy_state.execute_allowlist
    ));
    assert!(Arc::ptr_eq(
        &parent_guard.policy_state.safety_level,
        &child_guard.policy_state.safety_level
    ));
    assert!(child_guard.completion_state.command_cache.read().is_empty());
    assert!(
        child_guard
            .completion_state
            .executable_names
            .read()
            .is_empty()
    );
    assert_eq!(child_guard.session_output_state.output_history.len(), 0);
    assert!(
        !child_guard
            .policy_state
            .secret_manager
            .list_patterns()
            .iter()
            .any(|pattern| pattern == "CUSTOM_[A-Z]+")
    );
}

#[test]
fn lookup_caches_misses() {
    init();
    let env = Environment::new();
    let missing = "definitely-not-a-command-12345";

    assert_eq!(None, env.read().lookup(missing));
    assert_eq!(
        Some(&None),
        env.read()
            .completion_state
            .command_cache
            .read()
            .get(missing)
    );
}

#[test]
fn search_prefix_uses_prewarmed_names() {
    init();
    let env = Environment::new();
    env.write().set_executable_names(vec![
        "cargo".to_string(),
        "cat".to_string(),
        "git".to_string(),
    ]);

    assert_eq!(env.read().search_prefix("ca"), Some("cargo".to_string()));
}

#[test]
fn test_resolve_alias() {
    init();
    let env = Environment::new();
    env.write()
        .variable_state
        .alias
        .insert("ll".to_string(), "ls -la".to_string());

    // Test alias resolution
    let resolved = env.read().resolve_alias("ll");
    assert_eq!(resolved, "ls -la".to_string());

    // Test non-alias fallback
    let resolved = env.read().resolve_alias("unknown");
    assert_eq!(resolved, "unknown".to_string());
}

#[test]
fn api_key_presence_does_not_enable_ai_backfill() {
    init();
    let _guard = crate::test_env_lock();

    let keys = dsh_openai::API_KEY_ENV_VARS;
    let previous = keys.map(|key| std::env::var(key).ok());
    for key in keys {
        unsafe {
            std::env::remove_var(key);
        }
    }

    for key in keys {
        unsafe { std::env::set_var(key, "test-key") };
        let env = Environment::new();
        assert!(
            !env.read().suggestion_ai_enabled(),
            "{key} must not opt the user into background AI requests"
        );
        env.write().set_suggestion_ai_enabled(true);
        assert!(env.read().suggestion_ai_enabled());
        unsafe { std::env::remove_var(key) };
    }

    for (key, value) in keys.into_iter().zip(previous) {
        if let Some(value) = value {
            unsafe { std::env::set_var(key, value) };
        } else {
            unsafe { std::env::remove_var(key) };
        }
    }
}

#[test]
fn test_search() {
    init();
    let env = Environment::new();
    // Test absolute path
    let abs_path = "/usr/bin/env";
    if Path::new(abs_path).exists() {
        let p = env.read().search(abs_path);
        assert_eq!(Some(abs_path.to_string()), p);
    }

    // Test relative path (assumes running from repo root with Cargo.toml)
    let rel_path = "./Cargo.toml";
    if Path::new(rel_path).exists() {
        let p = env.read().search(rel_path);
        assert_eq!(Some(rel_path.to_string()), p);
    }

    // Test non-existent path
    let non_existent = "./non_existent_file_12345";
    let p = env.read().search(non_existent);
    assert_eq!(None, p);

    // Test command in PATH
    let p = env.read().search("ls");
    // Should find ls in one of the paths, usually /usr/bin/ls or /bin/ls
    // Note: search() via search_file() returns just the filename for PATH lookups
    assert!(p.is_some());
    assert_eq!(p.unwrap(), "ls");
}

#[test]
fn test_system_env_updates_refresh_path_and_child_env() {
    init();
    let env = Environment::new();

    {
        let mut guard = env.write();
        guard.set_system_env_var("PATH".to_string(), "/tmp/bin:/usr/bin".to_string());
        guard
            .variable_state
            .variables
            .insert("EXPORTED_ONLY".to_string(), "value".to_string());
        guard
            .variable_state
            .exported_vars
            .insert("EXPORTED_ONLY".to_string());
    }

    let guard = env.read();
    assert_eq!(
        guard.variable_state.paths,
        vec!["/tmp/bin".to_string(), "/usr/bin".to_string()]
    );

    let child_env = guard.child_process_env();
    assert_eq!(
        child_env.get("PATH"),
        Some(&"/tmp/bin:/usr/bin".to_string())
    );
    assert_eq!(child_env.get("EXPORTED_ONLY"), Some(&"value".to_string()));
}

#[test]
fn test_unset_system_env_updates_z_exclude() {
    init();
    let env = Environment::new();

    {
        let mut guard = env.write();
        guard.set_system_env_var("Z_EXCLUDE".to_string(), "/tmp:/var".to_string());
        assert_eq!(
            guard.variable_state.z_exclude,
            vec!["/tmp".to_string(), "/var".to_string()]
        );
        guard.unset_system_env_var("Z_EXCLUDE");
    }

    assert!(env.read().variable_state.z_exclude.is_empty());
}

/// Setting `AI_MESSAGE_LANG` has to reach the slot the AI service reads, not
/// just the variable map: the two used to be independent, so the setting
/// applied to the `!` runtime and to nothing else.
#[test]
fn setting_the_message_language_publishes_it_to_the_ai_service() {
    init();
    let env = Environment::new();

    {
        let mut guard = env.write();
        assert!(guard.integration_state.response_language.read().is_none());

        guard.set_shell_var("AI_MESSAGE_LANG".to_string(), "  Japanese  ".to_string());
        assert_eq!(
            guard.integration_state.response_language.read().clone(),
            Some("Japanese".to_string())
        );

        guard.set_shell_var("AI_MESSAGE_LANG".to_string(), "   ".to_string());
        assert!(guard.integration_state.response_language.read().is_none());
    }
}

/// `SAFETY_LEVEL` is read from the policy state, and the policy state is what
/// an inherited value has to reach: seeding the variable map with "normal"
/// unconditionally shadowed the environment the shell was started from.
#[test]
fn the_shell_starts_at_the_level_it_inherited() {
    init();
    let env = Environment::new();
    let guard = env.read();

    // `Environment::new` snapshots the process environment, so this asserts the
    // seeding path rather than a particular inherited value.
    let inherited = guard.variable_state.system_env_vars.get("SAFETY_LEVEL");
    let expected = crate::safety::SafetyLevel::from_env_value(inherited.cloned());

    assert_eq!(*guard.policy_state.safety_level.read(), expected);
    assert_eq!(
        guard.variable_state.variables.get("SAFETY_LEVEL"),
        Some(&expected.as_str().to_string())
    );
}
