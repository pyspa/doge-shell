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
    let baseline = env.read().variable_state.variables.len();
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

    // Parent sees its own insertion on top of the inherited baseline, but
    // not the child's later insertion.
    assert_eq!(baseline + 1, env1.read().variable_state.variables.len());
    assert!(!env1.read().variable_state.variables.contains_key("test2"));
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
            .insert("git".to_string(), "/usr/bin/git".to_string());
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
fn lookup_does_not_cache_misses() {
    init();
    let env = Environment::new();
    let missing = "definitely-not-a-command-12345";

    assert_eq!(None, env.read().lookup(missing));
    assert!(
        !env.read()
            .completion_state
            .command_cache
            .read()
            .contains_key(missing)
    );
}

fn write_mode_file(dir: &Path, name: &str, mode: u32) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, "#!/bin/sh\necho hi\n").unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(mode);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

fn env_with_paths(paths: Vec<String>) -> Arc<RwLock<Environment>> {
    let env = Environment::new();
    env.write().variable_state.paths = paths;
    env.write().completion_state.command_cache.write().clear();
    env
}

/// Restores the process cwd on drop, so a mid-test panic cannot leak a
/// tempdir cwd into concurrently running tests.
struct CwdGuard {
    previous: std::path::PathBuf,
}

impl CwdGuard {
    fn enter(dir: &Path) -> Self {
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        Self { previous }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous);
    }
}

#[test]
fn lookup_skips_non_executable_first_candidate() {
    init();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    write_mode_file(dir_a.path(), "foo", 0o644);
    let expected = write_mode_file(dir_b.path(), "foo", 0o755);

    let env = env_with_paths(vec![
        dir_a.path().display().to_string(),
        dir_b.path().display().to_string(),
    ]);
    assert_eq!(
        env.read().lookup("foo"),
        Some(expected.display().to_string())
    );
}

#[test]
fn lookup_revalidates_removed_cached_executable() {
    init();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let stale = write_mode_file(dir_a.path(), "foo", 0o755);
    let fallback = write_mode_file(dir_b.path(), "foo", 0o755);

    let env = env_with_paths(vec![
        dir_a.path().display().to_string(),
        dir_b.path().display().to_string(),
    ]);
    assert_eq!(env.read().lookup("foo"), Some(stale.display().to_string()));
    std::fs::remove_file(&stale).unwrap();
    assert_eq!(
        env.read().lookup("foo"),
        Some(fallback.display().to_string())
    );
}

#[test]
fn lookup_revalidates_de_executed_cached_executable() {
    init();
    use std::os::unix::fs::PermissionsExt;
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let stale = write_mode_file(dir_a.path(), "foo", 0o755);
    let fallback = write_mode_file(dir_b.path(), "foo", 0o755);

    let env = env_with_paths(vec![
        dir_a.path().display().to_string(),
        dir_b.path().display().to_string(),
    ]);
    assert_eq!(env.read().lookup("foo"), Some(stale.display().to_string()));
    let mut permissions = std::fs::metadata(&stale).unwrap().permissions();
    permissions.set_mode(0o644);
    std::fs::set_permissions(&stale, permissions).unwrap();
    assert_eq!(
        env.read().lookup("foo"),
        Some(fallback.display().to_string())
    );
}

#[test]
fn lookup_miss_then_install_is_found() {
    init();
    let dir = tempfile::tempdir().unwrap();
    let env = env_with_paths(vec![dir.path().display().to_string()]);
    assert_eq!(env.read().lookup("foo"), None);
    let expected = write_mode_file(dir.path(), "foo", 0o755);
    assert_eq!(
        env.read().lookup("foo"),
        Some(expected.display().to_string())
    );
}

#[test]
fn lookup_treats_every_slash_name_as_explicit_path() {
    init();
    let _guard = crate::test_env_lock();
    let work = tempfile::tempdir().unwrap();
    let sub = work.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let probe = write_mode_file(&sub, "probe", 0o755);
    let parent_probe = write_mode_file(work.path(), "parent-probe", 0o755);

    // `sub/foo` resolves literally even though PATH has nothing useful.
    let env = env_with_paths(vec!["/definitely/not/a/real/path".to_string()]);
    let _cwd = CwdGuard::enter(work.path());
    assert_eq!(
        env.read().lookup("sub/probe"),
        Some("sub/probe".to_string())
    );
    assert!(probe.exists());
    // `../foo` is also an explicit pathname, never a PATH search.
    std::env::set_current_dir(&sub).unwrap();
    assert_eq!(
        env.read().lookup("../parent-probe"),
        Some("../parent-probe".to_string())
    );
    assert!(parent_probe.exists());
    // A nested slash name is explicit too.
    assert_eq!(env.read().lookup("a/b/foo"), None);
}

#[test]
fn relative_path_entries_disable_persistent_command_cache() {
    init();
    use super::paths::path_lookup_is_cacheable;
    assert!(path_lookup_is_cacheable(&["/usr/bin".to_string()]));
    assert!(path_lookup_is_cacheable(&[
        "/a".to_string(),
        "/b".to_string()
    ]));
    assert!(!path_lookup_is_cacheable(&["".to_string()]));
    assert!(!path_lookup_is_cacheable(&[".".to_string()]));
    assert!(!path_lookup_is_cacheable(&["bin".to_string()]));
    assert!(!path_lookup_is_cacheable(&["../bin".to_string()]));
    assert!(!path_lookup_is_cacheable(&[
        "/usr/bin".to_string(),
        "bin".to_string()
    ]));

    // A relative PATH lookup resolves but is never remembered.
    let _guard = crate::test_env_lock();
    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    write_mode_file(&bin, "foo", 0o755);
    let env = env_with_paths(vec!["bin".to_string()]);
    let _cwd = CwdGuard::enter(work.path());
    assert_eq!(env.read().lookup("foo"), Some("bin/foo".to_string()));
    assert!(env.read().completion_state.command_cache.read().is_empty());
}

#[test]
fn same_value_path_assignment_invalidates_command_cache() {
    init();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let path_value = format!("{}:{}", dir_a.path().display(), dir_b.path().display());
    let late = dir_a.path().join("late-probe-xyz");
    let fallback = write_mode_file(dir_b.path(), "late-probe-xyz", 0o755);

    let env = Environment::new();
    {
        let mut guard = env.write();
        guard.set_shell_var("PATH".to_string(), path_value.clone());
    }
    assert_eq!(
        env.read().lookup("late-probe-xyz"),
        Some(fallback.display().to_string())
    );
    assert!(
        env.read()
            .completion_state
            .command_cache
            .read()
            .contains_key("late-probe-xyz")
    );
    // Install a preferred candidate, then assign the identical PATH value.
    write_mode_file(dir_a.path(), "late-probe-xyz", 0o755);
    {
        let mut guard = env.write();
        guard.set_shell_var("PATH".to_string(), path_value.clone());
    }
    assert!(
        env.read().completion_state.command_cache.read().is_empty()
            || !env
                .read()
                .completion_state
                .command_cache
                .read()
                .contains_key("late-probe-xyz")
    );
    assert_eq!(
        env.read().lookup("late-probe-xyz"),
        Some(late.display().to_string())
    );
}

#[test]
fn scoped_path_override_does_not_touch_persistent_cache() {
    init();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let in_a = write_mode_file(dir_a.path(), "foo", 0o755);
    let in_b = write_mode_file(dir_b.path(), "foo", 0o755);

    let env = env_with_paths(vec![dir_a.path().display().to_string()]);
    assert_eq!(env.read().lookup("foo"), Some(in_a.display().to_string()));
    // Override resolves from B without mutating or populating the cache.
    assert_eq!(
        env.read()
            .lookup_with_path_override("foo", Some(&dir_b.path().display().to_string())),
        Some(in_b.display().to_string())
    );
    assert_eq!(
        env.read().completion_state.command_cache.read().get("foo"),
        Some(&in_a.display().to_string())
    );
    // Duplicate scoped assignments keep last-wins, matching the child
    // environment the command runs with: resolve through the same
    // `Process::path_override` selection `resolve_program` uses.
    let dir_b_value = dir_b.path().display().to_string();
    let scoped = crate::process::Process::new("foo".to_string(), vec!["foo".to_string()])
        .with_execution_metadata(
            vec![],
            vec![
                ("PATH".to_string(), dir_a.path().display().to_string()),
                ("PATH".to_string(), dir_b_value.clone()),
            ],
        );
    assert_eq!(scoped.path_override(), Some(dir_b_value.as_str()));
    assert_eq!(
        env.read()
            .lookup_with_path_override("foo", scoped.path_override()),
        Some(in_b.display().to_string())
    );
    assert_eq!(
        env.read().completion_state.command_cache.read().get("foo"),
        Some(&in_a.display().to_string())
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
fn populated_executable_cache_does_not_fall_back_to_disk_on_miss() {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("late-command");
    std::fs::write(&executable, "").unwrap();
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&executable, permissions).unwrap();

    let env = Environment::new();
    let mut guard = env.write();
    guard.variable_state.paths = vec![dir.path().display().to_string()];
    guard.set_executable_names(vec!["cargo".to_string()]);

    assert_eq!(guard.search_prefix("late-"), None);
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
        guard.set_and_export_shell_var("PATH".to_string(), "/tmp/bin:/usr/bin".to_string());
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
fn reload_path_reactivates_a_directly_restored_path_snapshot() {
    let _guard = crate::test_env_lock();
    let environment = Environment::new();
    let path_a = vec!["/snapshot/a".to_string()];
    let path_b = vec!["/snapshot/b".to_string()];

    let scan_b = {
        let mut guard = environment.write();
        guard
            .variable_state
            .variables
            .insert("PATH".to_string(), path_b.join(":"));
        guard.variable_state.paths = path_b.clone();
        // Snapshot restore assigns the derived field directly, then asks the
        // normal projection refresh to reconcile global cache generations.
        guard.reload_path();
        let activation = crate::completion::generator::activate_system_command_cache(&path_b);
        crate::completion::generators::system::begin_system_command_scan(&activation)
    };

    {
        let mut guard = environment.write();
        guard
            .variable_state
            .variables
            .insert("PATH".to_string(), path_a.join(":"));
        guard.variable_state.paths = path_a.clone();
        guard.reload_path();
    }

    let stale_b_commands = ["b-command"].into_iter().map(String::from).collect();
    assert!(
        !crate::completion::generator::publish_system_command_scan(&scan_b, stale_b_commands),
        "pre-restore B worker retained authority after snapshot restored A"
    );
}

#[test]
fn test_unset_system_env_updates_z_exclude() {
    init();
    let env = Environment::new();

    {
        let mut guard = env.write();
        guard.set_shell_var("Z_EXCLUDE".to_string(), "/tmp:/var".to_string());
        assert_eq!(
            guard.variable_state.z_exclude,
            vec!["/tmp".to_string(), "/var".to_string()]
        );
        guard.unset_shell_var("Z_EXCLUDE");
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

/// Setting `AI_CHAT_MODEL` has to reach the slot `LiveAiService` and the
/// ghost-text backend read, not just the variable map - the same gap
/// `AI_MESSAGE_LANG` had before `reload_response_language` existed. An empty
/// value clears the override (falls back to the client's own default).
#[test]
fn setting_the_chat_model_publishes_it_to_the_ai_service() {
    init();
    let env = Environment::new();

    {
        let mut guard = env.write();
        assert!(guard.integration_state.chat_model.read().is_none());

        guard.set_shell_var("AI_CHAT_MODEL".to_string(), "  gpt-4o-mini  ".to_string());
        assert_eq!(
            guard.integration_state.chat_model.read().clone(),
            Some("gpt-4o-mini".to_string())
        );

        guard.set_shell_var("AI_CHAT_MODEL".to_string(), "".to_string());
        assert!(guard.integration_state.chat_model.read().is_none());
    }
}

/// The legacy `OPENAI_MODEL` alias is honored, the same way `OpenAiConfig`
/// honors it - but only as a fallback when `AI_CHAT_MODEL` was never set at
/// all: a *blank* `AI_CHAT_MODEL` does not fall through to `OPENAI_MODEL`
/// either here or in `OpenAiConfig::from_getter` (`getter("AI_CHAT_MODEL")
/// .or_else(|| getter("OPENAI_MODEL"))` short-circuits on `Some("")`). A
/// fresh environment - rather than blanking `AI_CHAT_MODEL` on the one from
/// the previous test - is what keeps this test exercising that fallback
/// instead of that quirk.
#[test]
fn the_legacy_model_variable_is_honored_as_a_fallback() {
    init();
    let env = Environment::new();
    env.write()
        .set_shell_var("OPENAI_MODEL".to_string(), "gpt-4".to_string());

    assert_eq!(
        env.read().integration_state.chat_model.read().clone(),
        Some("gpt-4".to_string())
    );
}

/// The model slot is shared, not copied: a subshell changing `AI_CHAT_MODEL`
/// must be visible to the parent's AI service too, the same property
/// `mcp_manager`/`response_language` already have (`extend_copies_shares_and_resets_state_by_group`).
#[test]
fn extend_shares_the_chat_model_slot() {
    init();
    let parent = Environment::new();
    let child = Environment::extend(parent.clone());

    assert!(Arc::ptr_eq(
        &parent.read().integration_state.chat_model,
        &child.read().integration_state.chat_model
    ));

    child
        .write()
        .set_shell_var("AI_CHAT_MODEL".to_string(), "gpt-4o-mini".to_string());

    assert_eq!(
        parent.read().integration_state.chat_model.read().clone(),
        Some("gpt-4o-mini".to_string())
    );
}

/// Rotating the key/endpoint at runtime has to reach the shared client slot:
/// `ChatGptClient` snapshots those at construction, so without a rebuild a
/// `vset AI_CHAT_API_KEY=...` only ever reached `!` chat (which resolves its
/// config per message) and never the palette, ghost text, or `ask_ai_async`.
#[test]
fn setting_the_api_key_rebuilds_the_shared_ai_client() {
    init();
    let env = Environment::new();

    // (No assumption about the inherited process environment here: it may
    // carry a key. Every step below sets an explicit shell value, which
    // `reload_ai_client` prefers over both the snapshot and the live
    // process environment.)
    env.write()
        .set_shell_var("AI_CHAT_API_KEY".to_string(), "fixture-key".to_string());
    assert!(env.read().ai_configured());

    // Rotating the key rebuilds rather than sticking with the first client.
    env.write()
        .set_shell_var("AI_CHAT_API_KEY".to_string(), "rotated-key".to_string());
    assert!(env.read().ai_configured());

    // An endpoint switch resolves into the same slot (no restart needed).
    env.write().set_shell_var(
        "AI_CHAT_BASE_URL".to_string(),
        "https://example.com/v1".to_string(),
    );
    assert!(env.read().ai_configured());

    // Clearing the key deconfigures shell-side AI again.
    env.write()
        .set_shell_var("AI_CHAT_API_KEY".to_string(), "".to_string());
    assert!(!env.read().ai_configured());
}

/// `SAFETY_LEVEL` is read from the policy state, and the policy state is what
/// an inherited value has to reach: seeding the variable map with "normal"
/// unconditionally shadowed the environment the shell was started from.
#[test]
fn the_shell_starts_at_the_level_it_inherited() {
    init();
    let env = Environment::new();
    let guard = env.read();

    // `Environment::new` imports the process environment into `variables`,
    // so this asserts the seeding path rather than a particular value.
    // `SAFETY_LEVEL` is seeded as "normal" when absent and never auto-exported.
    let inherited_exported = guard.variable_state.exported_vars.contains("SAFETY_LEVEL");
    let expected = crate::safety::SafetyLevel::from_env_value(
        guard
            .variable_state
            .variables
            .get("SAFETY_LEVEL")
            .and_then(|v| {
                // A fresh default "normal" that was not inherited must not count
                // as an inherited value.
                if !inherited_exported && v == "normal" {
                    None
                } else {
                    Some(v.clone())
                }
            }),
    );

    assert_eq!(*guard.policy_state.safety_level.read(), expected);
    assert_eq!(
        guard.variable_state.variables.get("SAFETY_LEVEL"),
        Some(&expected.as_str().to_string())
    );
}

/// `comp-gen` writes completion overrides to `$XDG_CONFIG_HOME/dogesh/completions`
/// (`dsh-builtin/src/completion_generation.rs`, `xdg::BaseDirectories::place_config_file`).
/// This function is what `dsh/src/completion/json_loader.rs` and
/// `dsh/src/output_schema/loader.rs` search to read them back, so its first
/// (most authoritative) entry must land in the same place a non-default
/// `XDG_CONFIG_HOME` sends the writer, or a generated completion is written
/// somewhere this function never looks.
#[test]
fn user_asset_override_dirs_honors_xdg_config_home() {
    init();
    let _guard = crate::test_env_lock();
    let dir = tempfile::tempdir().unwrap();

    let previous = std::env::var_os("XDG_CONFIG_HOME");
    unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };

    let dirs = user_asset_override_dirs("completions");

    match previous {
        Some(value) => unsafe { std::env::set_var("XDG_CONFIG_HOME", value) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }

    assert_eq!(
        dirs.first(),
        Some(&dir.path().join(APP_NAME).join("completions"))
    );
}

#[test]
fn bang_is_empty_before_any_async_launch() {
    init();
    let env = Environment::new();
    assert_eq!(env.read().lookup_variable("!"), Some(String::new()));
}

#[test]
fn bang_reports_last_async_pid() {
    init();
    let env = Environment::new();
    env.write().last_async_pid = Some(1234);
    assert_eq!(env.read().lookup_variable("!"), Some("1234".to_string()));
}

#[test]
fn user_variable_cannot_shadow_bang() {
    init();
    let env = Environment::new();
    env.write()
        .variable_state
        .variables
        .insert("!".to_string(), "shadowed".to_string());
    env.write().last_async_pid = Some(4242);
    // Special parameters resolve before user variables, like `?` and `$`.
    assert_eq!(env.read().lookup_variable("!"), Some("4242".to_string()));
}

#[test]
fn shell_options_default_is_pipefail_off() {
    use dsh_types::shell_options::ShellOption;
    init();
    let env = Environment::new();
    assert!(!env.read().shell_options.enabled(ShellOption::Pipefail));
}

#[test]
fn extend_copies_shell_options_without_sharing() {
    use dsh_types::shell_options::ShellOption;
    init();
    let parent = Environment::new();
    parent
        .write()
        .shell_options
        .set(ShellOption::Pipefail, true);
    let child = Environment::extend(parent.clone());
    assert!(child.read().shell_options.enabled(ShellOption::Pipefail));
    // `ShellOptions` is `Copy`: flipping the child must not flip the parent.
    child
        .write()
        .shell_options
        .set(ShellOption::Pipefail, false);
    assert!(parent.read().shell_options.enabled(ShellOption::Pipefail));
    assert!(!child.read().shell_options.enabled(ShellOption::Pipefail));
}

/// Shell variable storage holds bare names only: `FOO`, `$FOO`, `${FOO}`
/// spellings collapse to one `FOO` key, never two entries.
#[test]
fn shell_var_setters_canonicalize_to_bare_names() {
    init();
    let env = Environment::new();
    {
        let mut guard = env.write();
        guard.set_shell_var("FOO".to_string(), "a".to_string());
        guard.set_shell_var("$FOO".to_string(), "b".to_string());
        guard.set_shell_var("${FOO}".to_string(), "c".to_string());
    }
    let guard = env.read();
    assert_eq!(
        guard.variable_state.variables.get("FOO"),
        Some(&"c".to_string())
    );
    assert!(!guard.variable_state.variables.contains_key("$FOO"));
    assert!(!guard.variable_state.variables.contains_key("${FOO}"));
    assert!(
        guard
            .variable_state
            .variables
            .keys()
            .filter(|k| k.contains("FOO"))
            .count()
            == 1,
        "duplicate FOO representations: {:?}",
        guard.variable_state.variables.keys().collect::<Vec<_>>()
    );
    // All three input spellings still resolve.
    assert_eq!(guard.get_var("FOO"), Some("c".to_string()));
    assert_eq!(guard.get_var("$FOO"), Some("c".to_string()));
    assert_eq!(guard.get_var("${FOO}"), Some("c".to_string()));
    assert_eq!(guard.lookup_variable("$FOO"), Some("c".to_string()));
    assert_eq!(guard.lookup_variable("${FOO}"), Some("c".to_string()));
}

/// `exported_vars` is bare names only as well.
#[test]
fn export_markers_canonicalize_to_bare_names() {
    init();
    let env = Environment::new();
    {
        let mut guard = env.write();
        guard.set_shell_var("FOO".to_string(), "v".to_string());
        guard.export_shell_var("$FOO".to_string());
    }
    let guard = env.read();
    assert!(guard.variable_state.exported_vars.contains("FOO"));
    assert!(!guard.variable_state.exported_vars.contains("$FOO"));
    assert_eq!(guard.child_process_env().get("FOO"), Some(&"v".to_string()));
}

/// An empty name never aliases the PID special.
#[test]
fn empty_name_does_not_alias_the_pid_special() {
    init();
    use super::variables::canonical_shell_var_name;
    assert_eq!(canonical_shell_var_name(""), "");
    assert_eq!(canonical_shell_var_name("$"), "$");
    assert_eq!(canonical_shell_var_name("$$"), "$");
    let env = Environment::new();
    assert!(env.read().lookup_variable("").is_none());
}

// --- Phase 0: single-namespace regressions ---

/// Inherited process environment lands in `variables` as exported.
#[test]
fn inherited_environment_stays_exported() {
    init();
    let _guard = crate::test_env_lock();
    let name = "DOGESH_TEST_INHERITED_EXPORT";
    let value = "inherited-value";
    let previous = std::env::var_os(name);
    unsafe { std::env::set_var(name, value) };

    let env = Environment::new();
    let guard = env.read();
    assert_eq!(guard.lookup_variable(name), Some(value.to_string()));
    assert_eq!(
        guard.variable_state.variables.get(name),
        Some(&value.to_string())
    );
    assert!(guard.variable_state.exported_vars.contains(name));
    assert_eq!(
        guard.child_process_env().get(name),
        Some(&value.to_string())
    );

    drop(guard);
    match previous {
        Some(v) => unsafe { std::env::set_var(name, v) },
        None => unsafe { std::env::remove_var(name) },
    }
}

/// `export INHERITED` must not drop the value from the child environment.
#[test]
fn re_export_inherited_value_keeps_child() {
    init();
    let env = Environment::new();
    {
        let mut guard = env.write();
        guard.variable_state.variables.clear();
        guard.variable_state.exported_vars.clear();
        // Simulate an inherited variable.
        guard.set_and_export_shell_var("SOME_INHERITED_VAR".to_string(), "original".to_string());
        // Re-export: value absent from a second store cannot happen any more,
        // but the operation itself must keep the child value.
        guard.export_shell_var("SOME_INHERITED_VAR".to_string());
    }
    let guard = env.read();
    assert_eq!(
        guard.lookup_variable("SOME_INHERITED_VAR"),
        Some("original".to_string())
    );
    assert_eq!(
        guard.child_process_env().get("SOME_INHERITED_VAR"),
        Some(&"original".to_string())
    );
}

/// `set FOO changed` on an inherited (exported) name keeps the export bit.
#[test]
fn assignment_preserves_inherited_export_attribute() {
    init();
    let env = Environment::new();
    {
        let mut guard = env.write();
        guard.variable_state.variables.clear();
        guard.variable_state.exported_vars.clear();
        guard.set_and_export_shell_var("FOO".to_string(), "inherited".to_string());
        guard.set_shell_var("FOO".to_string(), "changed".to_string());
    }
    let guard = env.read();
    assert_eq!(guard.lookup_variable("FOO"), Some("changed".to_string()));
    assert_eq!(
        guard.child_process_env().get("FOO"),
        Some(&"changed".to_string())
    );
    assert!(guard.variable_state.exported_vars.contains("FOO"));
}

/// Legacy `set -x` updates the single logical value.
#[test]
fn legacy_set_x_updates_one_logical_value() {
    init();
    let env = Environment::new();
    {
        let mut guard = env.write();
        guard.variable_state.variables.clear();
        guard.variable_state.exported_vars.clear();
        guard.set_shell_var("FOO".to_string(), "old".to_string());
        // `set -x FOO new` routes through `set_and_export_shell_var`.
        guard.set_and_export_shell_var("FOO".to_string(), "new".to_string());
    }
    let guard = env.read();
    assert_eq!(
        guard.variable_state.variables.get("FOO"),
        Some(&"new".to_string())
    );
    assert_eq!(guard.lookup_variable("FOO"), Some("new".to_string()));
    assert_eq!(
        guard.child_process_env().get("FOO"),
        Some(&"new".to_string())
    );
}

/// Logical unset removes value, export bit, and child entry together.
#[test]
fn unset_exported_variable_removes_everything() {
    init();
    let env = Environment::new();
    {
        let mut guard = env.write();
        guard.variable_state.variables.clear();
        guard.variable_state.exported_vars.clear();
        guard.set_and_export_shell_var("FOO".to_string(), "value".to_string());
        guard.unset_shell_var("FOO");
    }
    let guard = env.read();
    assert!(!guard.variable_state.variables.contains_key("FOO"));
    assert!(!guard.variable_state.exported_vars.contains("FOO"));
    assert!(!guard.child_process_env().contains_key("FOO"));
    assert_eq!(guard.lookup_variable("FOO"), None);
}

/// Export semantics matrix (§56): one logical name, one value, export bit.
#[test]
fn export_semantics_regression_matrix() {
    init();
    // inherited FOO=a, no op -> shell a, child a, on
    {
        let env = Environment::new();
        {
            let mut g = env.write();
            g.variable_state.variables.clear();
            g.variable_state.exported_vars.clear();
            g.set_and_export_shell_var("FOO".to_string(), "a".to_string());
        }
        let g = env.read();
        assert_eq!(g.lookup_variable("FOO"), Some("a".to_string()));
        assert_eq!(g.child_process_env().get("FOO"), Some(&"a".to_string()));
        assert!(g.variable_state.exported_vars.contains("FOO"));
    }
    // inherited FOO=a, `set FOO b` -> b/b/on
    {
        let env = Environment::new();
        {
            let mut g = env.write();
            g.variable_state.variables.clear();
            g.variable_state.exported_vars.clear();
            g.set_and_export_shell_var("FOO".to_string(), "a".to_string());
            g.set_shell_var("FOO".to_string(), "b".to_string());
        }
        let g = env.read();
        assert_eq!(g.lookup_variable("FOO"), Some("b".to_string()));
        assert_eq!(g.child_process_env().get("FOO"), Some(&"b".to_string()));
        assert!(g.variable_state.exported_vars.contains("FOO"));
    }
    // absent, `set FOO b` -> b/absent/off
    {
        let env = Environment::new();
        {
            let mut g = env.write();
            g.variable_state.variables.clear();
            g.variable_state.exported_vars.clear();
            g.set_shell_var("FOO".to_string(), "b".to_string());
        }
        let g = env.read();
        assert_eq!(g.lookup_variable("FOO"), Some("b".to_string()));
        assert!(!g.child_process_env().contains_key("FOO"));
        assert!(!g.variable_state.exported_vars.contains("FOO"));
    }
    // absent, `export FOO=b` -> b/b/on
    {
        let env = Environment::new();
        {
            let mut g = env.write();
            g.variable_state.variables.clear();
            g.variable_state.exported_vars.clear();
            g.set_and_export_shell_var("FOO".to_string(), "b".to_string());
        }
        let g = env.read();
        assert_eq!(g.lookup_variable("FOO"), Some("b".to_string()));
        assert_eq!(g.child_process_env().get("FOO"), Some(&"b".to_string()));
        assert!(g.variable_state.exported_vars.contains("FOO"));
    }
    // local FOO=a, `export FOO` -> a/a/on
    {
        let env = Environment::new();
        {
            let mut g = env.write();
            g.variable_state.variables.clear();
            g.variable_state.exported_vars.clear();
            g.set_shell_var("FOO".to_string(), "a".to_string());
            g.export_shell_var("FOO".to_string());
        }
        let g = env.read();
        assert_eq!(g.lookup_variable("FOO"), Some("a".to_string()));
        assert_eq!(g.child_process_env().get("FOO"), Some(&"a".to_string()));
        assert!(g.variable_state.exported_vars.contains("FOO"));
    }
    // exported FOO=a, logical unset -> absent/absent/off
    {
        let env = Environment::new();
        {
            let mut g = env.write();
            g.variable_state.variables.clear();
            g.variable_state.exported_vars.clear();
            g.set_and_export_shell_var("FOO".to_string(), "a".to_string());
            g.unset_shell_var("FOO");
        }
        let g = env.read();
        assert_eq!(g.lookup_variable("FOO"), None);
        assert!(!g.child_process_env().contains_key("FOO"));
        assert!(!g.variable_state.exported_vars.contains("FOO"));
    }
}

/// `Environment::child_process_env` and `Process::prepare_execution` agree.
#[test]
fn child_execution_consistency() {
    init();
    use crate::process::Process;
    let env = Environment::new();
    {
        let mut g = env.write();
        g.variable_state.variables.clear();
        g.variable_state.exported_vars.clear();
        g.set_and_export_shell_var("SHARED".to_string(), "shared".to_string());
        g.set_shell_var("LOCAL_ONLY".to_string(), "local".to_string());
        g.set_and_export_shell_var("TERM".to_string(), "dumb".to_string());
    }
    let expected = env.read().child_process_env();

    let process = Process::new("echo".to_string(), vec!["echo".to_string()]);
    let prepared = process
        .prepare_execution(env.clone())
        .expect("prepare execution");
    let mut actual = std::collections::HashMap::new();
    for entry in &prepared.envp {
        let text = entry.to_str().expect("env CString");
        let (k, v) = text.split_once('=').expect("KEY=VALUE");
        actual.insert(k.to_string(), v.to_string());
    }
    for (key, value) in &expected {
        assert_eq!(
            actual.get(key),
            Some(value),
            "child_process_env and prepare_execution disagree on {key}"
        );
    }
    assert!(!actual.contains_key("LOCAL_ONLY"));
    // No command-scoped overrides here, so the sets match exactly.
    assert_eq!(actual.len(), expected.len());

    // Command-scoped overrides win and appear once.
    let scoped = Process::new("echo".to_string(), vec!["echo".to_string()])
        .with_execution_metadata(vec![], vec![("SHARED".to_string(), "override".to_string())]);
    let prepared = scoped
        .prepare_execution(env.clone())
        .expect("prepare scoped");
    let scoped_map: std::collections::HashMap<String, String> = prepared
        .envp
        .iter()
        .map(|e| {
            let text = e.to_str().unwrap();
            let (k, v) = text.split_once('=').unwrap();
            (k.to_string(), v.to_string())
        })
        .collect();
    assert_eq!(scoped_map.get("SHARED"), Some(&"override".to_string()));
    assert_eq!(
        scoped_map.values().filter(|v| *v == "override").count(),
        1,
        "duplicate override entries: {scoped_map:?}"
    );
}

/// Unsetting a runtime AI key must not resurrect it from the process env.
#[test]
fn ai_unset_does_not_resurrect_from_process_env() {
    init();
    let _guard = crate::test_env_lock();
    let key = "AI_CHAT_API_KEY";
    let previous = std::env::var_os(key);
    unsafe { std::env::set_var(key, "process-global-key") };

    let env = Environment::new();
    // Startup import sees the key.
    assert!(env.read().lookup_variable(key).is_some());
    {
        let mut guard = env.write();
        guard.unset_shell_var(key);
        guard.refresh_derived_state(key);
    }
    assert_eq!(env.read().lookup_variable(key), None);
    assert!(
        !env.read().ai_configured(),
        "unset shell key must not be revived from std::env"
    );

    match previous {
        Some(v) => unsafe { std::env::set_var(key, v) },
        None => unsafe { std::env::remove_var(key) },
    }
}

/// PATH entries are kept as strings: relative entries are not absolutized,
/// duplicates are not removed, and non-existent directories are accepted.
#[test]
fn insert_path_entry_keeps_entries_verbatim() {
    init();
    let env = Environment::new();
    env.write()
        .set_shell_var("PATH".to_string(), "/a:/b".to_string());

    {
        let mut guard = env.write();
        guard.insert_path_entry(0, "./bin");
        guard.insert_path_entry(0, "/a");
        guard.insert_path_entry(0, "/future/toolchain/bin");
    }

    let guard = env.read();
    assert_eq!(
        guard.lookup_variable("PATH"),
        Some("/future/toolchain/bin:/a:./bin:/a:/b".to_string())
    );
    assert_eq!(
        guard.variable_state.paths,
        vec![
            "/future/toolchain/bin".to_string(),
            "/a".to_string(),
            "./bin".to_string(),
            "/a".to_string(),
            "/b".to_string()
        ]
    );
}

/// `insert_path_entry` rewrites the logical `PATH` variable through the
/// canonical setter, so the variable and the derived lookup projection
/// agree afterwards.
#[test]
fn insert_path_entry_updates_logical_path_and_projection() {
    init();
    let env = Environment::new();
    env.write()
        .set_shell_var("PATH".to_string(), "/old/a:/old/b".to_string());

    env.write().insert_path_entry(0, "/new");

    let guard = env.read();
    assert_eq!(
        guard.lookup_variable("PATH"),
        Some("/new:/old/a:/old/b".to_string())
    );
    assert_eq!(
        guard.variable_state.paths,
        vec![
            "/new".to_string(),
            "/old/a".to_string(),
            "/old/b".to_string()
        ]
    );
}

/// An exported `PATH` stays exported, and the child environment sees the
/// new value. `add_path` must never be an implicit `export`.
#[test]
fn insert_path_entry_preserves_exported_path() {
    init();
    let env = Environment::new();
    {
        let mut guard = env.write();
        guard.set_shell_var("PATH".to_string(), "/old".to_string());
        guard.export_shell_var("PATH".to_string());
    }

    env.write().insert_path_entry(0, "/new");

    let guard = env.read();
    assert!(guard.variable_state.exported_vars.contains("PATH"));
    assert_eq!(
        guard.child_process_env().get("PATH"),
        Some(&"/new:/old".to_string())
    );
}

/// An unexported `PATH` stays unexported: `add_path` materializes the new
/// logical value without adding the export attribute.
#[test]
fn insert_path_entry_does_not_export_an_unexported_path() {
    init();
    let env = Environment::new();
    {
        let mut guard = env.write();
        guard.unset_shell_var("PATH");
        guard.set_shell_var("PATH".to_string(), "/old".to_string());
    }
    assert!(!env.read().variable_state.exported_vars.contains("PATH"));

    env.write().insert_path_entry(0, "/new");

    let guard = env.read();
    assert_eq!(guard.lookup_variable("PATH"), Some("/new:/old".to_string()));
    assert_eq!(
        guard.variable_state.paths,
        vec!["/new".to_string(), "/old".to_string()]
    );
    assert!(!guard.variable_state.exported_vars.contains("PATH"));
    assert!(!guard.child_process_env().contains_key("PATH"));
}

/// With `PATH` logically unset, the fallback projection is materialized
/// into a new logical value instead of leaving the variable absent.
#[test]
fn insert_path_entry_materializes_fallback_when_path_is_unset() {
    init();
    let env = Environment::new();
    env.write().unset_shell_var("PATH");
    assert_eq!(env.read().lookup_variable("PATH"), None);

    env.write().insert_path_entry(0, "/custom/bin");

    let guard = env.read();
    let logical = guard.lookup_variable("PATH").expect("PATH is materialized");
    assert!(
        logical.starts_with("/custom/bin:"),
        "unexpected logical PATH: {logical}"
    );
    assert!(logical.contains("/usr/bin"));
    assert_eq!(guard.variable_state.paths[0], "/custom/bin".to_string());
    assert!(!guard.variable_state.exported_vars.contains("PATH"));
}

/// Prepending a directory invalidates the remembered command location, so
/// the new directory's candidate wins, and bumps the PATH generation that
/// scopes completion caches.
#[test]
fn insert_path_entry_invalidates_command_cache_and_bumps_generation() {
    init();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let in_b = write_mode_file(dir_b.path(), "foo", 0o755);
    let in_a = write_mode_file(dir_a.path(), "foo", 0o755);

    let env = Environment::new();
    env.write()
        .set_shell_var("PATH".to_string(), dir_b.path().display().to_string());
    assert_eq!(env.read().lookup("foo"), Some(in_b.display().to_string()));
    assert!(
        env.read()
            .completion_state
            .command_cache
            .read()
            .contains_key("foo"),
        "expected a positive cache entry before the PATH mutation"
    );

    let before = env.read().completion_state.path_generation;
    env.write()
        .insert_path_entry(0, &dir_a.path().display().to_string());
    let after = env.read().completion_state.path_generation;
    assert!(after > before, "PATH generation did not advance");

    assert_eq!(env.read().lookup("foo"), Some(in_a.display().to_string()));
}

/// `~/...` entries resolve against the logical shell `HOME`, not the
/// process-global one.
#[test]
fn insert_path_entry_uses_logical_home_for_tilde() {
    init();
    let _guard = crate::test_env_lock();
    let _stale = crate::ProcessEnvGuard::set("HOME", "/stale/process/home");

    let env = Environment::new();
    env.write()
        .set_shell_var("PATH".to_string(), "/old".to_string());
    env.write()
        .set_shell_var("HOME".to_string(), "/logical/home".to_string());

    env.write().insert_path_entry(0, "~/bin");

    let guard = env.read();
    assert_eq!(
        guard.lookup_variable("PATH"),
        Some("/logical/home/bin:/old".to_string())
    );
    assert_eq!(
        guard.variable_state.paths[0],
        "/logical/home/bin".to_string()
    );
}
