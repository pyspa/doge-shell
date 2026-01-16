//! Tests for the environment module.

use super::*;
use std::path::Path;

fn init() {
    let _ = tracing_subscriber::fmt::try_init();
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
        .variables
        .insert("test".to_string(), "value".to_string());

    let env2 = Environment::extend(env);
    let env2_clone = Arc::clone(&env2);

    env2.write()
        .variables
        .insert("test2".to_string(), "value2".to_string());

    let env2_clone = env2_clone.read();
    let v = env2_clone.variables.get("test");
    assert_eq!("value".to_string(), *v.unwrap());
    assert_eq!(
        "value2".to_string(),
        *env2_clone.variables.get("test2").unwrap()
    );

    assert_eq!(2, env1.read().variables.len());
}

#[test]
fn test_resolve_alias() {
    init();
    let env = Environment::new();
    env.write()
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
fn auto_enables_ai_backfill_when_api_key_present() {
    init();

    let key = "AI_CHAT_API_KEY";
    let previous = std::env::var(key).ok();
    unsafe {
        std::env::set_var(key, "test-key");
    }

    let prefs = super::default_input_preferences();
    assert!(
        prefs.ai_backfill,
        "AI suggestions should auto-enable when key is set"
    );

    if let Some(value) = previous {
        unsafe {
            std::env::set_var(key, value);
        }
    } else {
        unsafe {
            std::env::remove_var(key);
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
