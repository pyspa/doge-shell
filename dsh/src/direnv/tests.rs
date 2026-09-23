use super::*;
use crate::environment::Environment;
use std::sync::Arc;

fn write_root_file(dir: &tempfile::TempDir, name: &str, content: &str) {
    fs::write(dir.path().join(name), content).unwrap();
}

fn register_root(env: &mut Environment, dir: &tempfile::TempDir) {
    register_root_at(env, dir.path());
}

fn register_root_at(env: &mut Environment, path: &std::path::Path) {
    let root = DirEnvironment::new(path.to_str().unwrap().to_string());
    env.variable_state.direnv_roots.push(root);
}

fn is_root_active(env: &Environment, path: &std::path::Path) -> bool {
    let key = path.to_str().unwrap();
    env.variable_state
        .direnv_roots
        .iter()
        .find(|root| root.path == key)
        .map(|root| root.is_active())
        .unwrap_or(false)
}

fn root_restore_len(env: &Environment, path: &std::path::Path) -> usize {
    let key = path.to_str().unwrap();
    env.variable_state
        .direnv_roots
        .iter()
        .find(|root| root.path == key)
        .map(|root| root.restore_len())
        .unwrap_or(usize::MAX)
}

fn system_env(env: &Environment, key: &str) -> Option<String> {
    env.variable_state.system_env_vars.get(key).cloned()
}

fn enter(shell_env: &Arc<RwLock<Environment>>, dir: &tempfile::TempDir) {
    check_path(dir.path(), Arc::clone(shell_env)).unwrap();
}

fn leave(shell_env: &Arc<RwLock<Environment>>, outside: &tempfile::TempDir) {
    check_path(outside.path(), Arc::clone(shell_env)).unwrap();
}

#[test]
fn test_dir_environment_creation() {
    let path = "/tmp/test".to_string();

    let dir_env = DirEnvironment::new(path.clone());
    assert_eq!(dir_env.path, path);
    assert!(!dir_env.is_active());
}

#[test]
fn test_dir_environment_basic_functionality() {
    let dir_env = DirEnvironment::new("/tmp/test".to_string());

    // Basic functionality test
    assert_eq!(dir_env.path, "/tmp/test");
    assert!(!dir_env.is_active());
}
#[test]
fn test_read_envrc_quotes() -> Result<()> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new()?;
    writeln!(file, "export QUOTED=\"value with spaces\"")?;
    writeln!(file, "export SINGLE='single quoted'")?;

    let path = file.path().to_str().unwrap();
    let entries = read_envrc_config_file(path)?;

    let mut found_quoted = false;
    let mut found_single = false;

    for entry in entries {
        if let Entry::Env(e) = entry {
            if e.key == "QUOTED" {
                assert_eq!(e.value, "value with spaces"); // Expect quotes stripped
                found_quoted = true;
            } else if e.key == "SINGLE" {
                assert_eq!(e.value, "single quoted"); // Expect quotes stripped
                found_single = true;
            }
        }
    }
    assert!(found_quoted);
    assert!(found_single);
    Ok(())
}

#[test]
fn test_read_env_config_file_skips_comments_and_blank_lines() -> Result<()> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new()?;
    writeln!(file, "# a comment")?;
    writeln!(file)?;
    writeln!(file, "FOO=bar")?;

    let path = file.path().to_str().unwrap();
    let entries = read_env_config_file(path)?;

    assert_eq!(entries.len(), 1);
    match &entries[0] {
        Entry::Env(e) => {
            assert_eq!(e.key, "FOO");
            assert_eq!(e.value, "bar");
        }
        _ => panic!("expected env entry"),
    }
    Ok(())
}

#[test]
fn test_read_envrc_config_file_skips_blank_and_malformed_lines() -> Result<()> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new()?;
    writeln!(file)?;
    writeln!(file, "# comment")?;
    writeln!(file, "PATH_ADD /usr/local/bin")?;
    writeln!(file, "NOT_A_DIRECTIVE")?;
    writeln!(file, "export")?;

    let path = file.path().to_str().unwrap();
    let entries = read_envrc_config_file(path)?;

    assert_eq!(entries.len(), 1);
    assert!(matches!(entries[0], Entry::PathAdd(_)));
    Ok(())
}

#[test]
fn test_prepend_path_entry_joins_with_separator() {
    assert_eq!(
        prepend_path_entry("/usr/local/bin", "/usr/bin:/bin"),
        "/usr/local/bin:/usr/bin:/bin"
    );
}

#[test]
fn test_prepend_path_entry_on_empty_path() {
    assert_eq!(prepend_path_entry("/usr/local/bin", ""), "/usr/local/bin");
}

// --- Phase 0 regression: transactional overlay / restore ---

#[test]
fn existing_value_is_restored_on_leave() {
    let project = tempfile::tempdir().unwrap();
    write_root_file(&project, ".env", "FOO=project\n");
    let outside = tempfile::tempdir().unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.unset_system_env_var("FOO");
        env.set_system_env_var("FOO".to_string(), "original".to_string());
        register_root(&mut env, &project);
    }

    enter(&shell_env, &project);
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("project".to_string())
    );

    leave(&shell_env, &outside);
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("original".to_string())
    );
}

#[test]
fn originally_absent_variable_is_absent_after_leave() {
    let project = tempfile::tempdir().unwrap();
    write_root_file(&project, ".env", "FOO=project\n");
    let outside = tempfile::tempdir().unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.unset_system_env_var("FOO");
        register_root(&mut env, &project);
    }

    enter(&shell_env, &project);
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("project".to_string())
    );

    leave(&shell_env, &outside);
    assert_eq!(system_env(&shell_env.read(), "FOO"), None);
}

#[test]
fn path_restore_uses_activation_time_state() {
    let project = tempfile::tempdir().unwrap();
    write_root_file(&project, ".envrc", "PATH_ADD /project/bin\n");
    let outside = tempfile::tempdir().unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        // Register first, then change PATH: the restore point must be the
        // activation-time PATH, not the registration-time one.
        register_root(&mut env, &project);
        env.set_system_env_var("PATH".to_string(), "/new".to_string());
    }

    enter(&shell_env, &project);
    assert_eq!(
        system_env(&shell_env.read(), "PATH"),
        Some("/project/bin:/new".to_string())
    );

    leave(&shell_env, &outside);
    assert_eq!(
        system_env(&shell_env.read(), "PATH"),
        Some("/new".to_string())
    );
}

#[test]
fn dotenv_path_assignment_is_not_overwritten() {
    let project = tempfile::tempdir().unwrap();
    write_root_file(&project, ".env", "PATH=/project\n");
    let outside = tempfile::tempdir().unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.set_system_env_var("PATH".to_string(), "/base".to_string());
        register_root(&mut env, &project);
    }

    enter(&shell_env, &project);
    assert_eq!(
        system_env(&shell_env.read(), "PATH"),
        Some("/project".to_string())
    );

    leave(&shell_env, &outside);
    assert_eq!(
        system_env(&shell_env.read(), "PATH"),
        Some("/base".to_string())
    );
}

#[test]
fn envrc_export_then_path_add_composes_in_file_order() {
    let project = tempfile::tempdir().unwrap();
    write_root_file(&project, ".envrc", "export PATH=/custom\nPATH_ADD /extra\n");
    let outside = tempfile::tempdir().unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.set_system_env_var("PATH".to_string(), "/base".to_string());
        register_root(&mut env, &project);
    }

    enter(&shell_env, &project);
    assert_eq!(
        system_env(&shell_env.read(), "PATH"),
        Some("/extra:/custom".to_string())
    );

    leave(&shell_env, &outside);
    assert_eq!(
        system_env(&shell_env.read(), "PATH"),
        Some("/base".to_string())
    );
}

#[test]
fn envrc_path_add_then_export_keeps_file_order() {
    let project = tempfile::tempdir().unwrap();
    write_root_file(&project, ".envrc", "PATH_ADD /extra\nexport PATH=/custom\n");
    let outside = tempfile::tempdir().unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.set_system_env_var("PATH".to_string(), "/base".to_string());
        register_root(&mut env, &project);
    }

    enter(&shell_env, &project);
    assert_eq!(
        system_env(&shell_env.read(), "PATH"),
        Some("/custom".to_string())
    );

    leave(&shell_env, &outside);
    assert_eq!(
        system_env(&shell_env.read(), "PATH"),
        Some("/base".to_string())
    );
}

// --- Phase 9: nested / sibling / failure semantics ---

fn setup_nested_reversed() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    tempfile::TempDir,
    Arc<RwLock<Environment>>,
) {
    let outer = tempfile::tempdir().unwrap();
    write_root_file(&outer, ".env", "FOO=outer\n");
    let inner_path = outer.path().join("sub");
    fs::create_dir(&inner_path).unwrap();
    fs::write(inner_path.join(".env"), "FOO=inner\n").unwrap();
    let outside = tempfile::tempdir().unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.unset_system_env_var("FOO");
        env.set_system_env_var("FOO".to_string(), "base".to_string());
        // Intentionally reversed: inner registered before outer.
        register_root_at(&mut env, &inner_path);
        register_root_at(&mut env, outer.path());
    }
    (outer, inner_path, outside, shell_env)
}

#[test]
fn nested_direct_entry_loads_shallowest_first() {
    let (outer, inner_path, _outside, shell_env) = setup_nested_reversed();

    check_path(&inner_path, Arc::clone(&shell_env)).unwrap();
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("inner".to_string())
    );
    assert!(is_root_active(&shell_env.read(), outer.path()));
    assert!(is_root_active(&shell_env.read(), &inner_path));
}

#[test]
fn nested_leave_inner_restores_outer() {
    let (outer, inner_path, _outside, shell_env) = setup_nested_reversed();

    check_path(&inner_path, Arc::clone(&shell_env)).unwrap();
    check_path(outer.path(), Arc::clone(&shell_env)).unwrap();
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("outer".to_string())
    );
    assert!(is_root_active(&shell_env.read(), outer.path()));
    assert!(!is_root_active(&shell_env.read(), &inner_path));
}

#[test]
fn nested_full_leave_restores_base() {
    let (_outer, inner_path, outside, shell_env) = setup_nested_reversed();

    check_path(&inner_path, Arc::clone(&shell_env)).unwrap();
    leave(&shell_env, &outside);
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("base".to_string())
    );
    assert!(
        shell_env
            .read()
            .variable_state
            .direnv_roots
            .iter()
            .all(|root| !root.is_active())
    );
}

#[test]
fn sibling_transition_unloads_before_loading() {
    let dir_a = tempfile::tempdir().unwrap();
    write_root_file(&dir_a, ".env", "FOO=A\n");
    let dir_b = tempfile::tempdir().unwrap();
    write_root_file(&dir_b, ".env", "FOO=B\n");
    let outside = tempfile::tempdir().unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.unset_system_env_var("FOO");
        env.set_system_env_var("FOO".to_string(), "base".to_string());
        register_root(&mut env, &dir_a);
        register_root(&mut env, &dir_b);
    }

    enter(&shell_env, &dir_a);
    assert_eq!(system_env(&shell_env.read(), "FOO"), Some("A".to_string()));

    // B must snapshot the restored base, not A's overlay.
    enter(&shell_env, &dir_b);
    assert_eq!(system_env(&shell_env.read(), "FOO"), Some("B".to_string()));
    assert!(!is_root_active(&shell_env.read(), dir_a.path()));

    leave(&shell_env, &outside);
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("base".to_string())
    );
}

#[test]
fn path_add_nested_composes_and_unwinds() {
    let outer = tempfile::tempdir().unwrap();
    write_root_file(&outer, ".envrc", "PATH_ADD /outer\n");
    let inner_path = outer.path().join("sub");
    fs::create_dir(&inner_path).unwrap();
    fs::write(inner_path.join(".envrc"), "PATH_ADD /inner\n").unwrap();
    let outside = tempfile::tempdir().unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.set_system_env_var("PATH".to_string(), "/base".to_string());
        register_root_at(&mut env, &inner_path);
        register_root_at(&mut env, outer.path());
    }

    check_path(&inner_path, Arc::clone(&shell_env)).unwrap();
    assert_eq!(
        system_env(&shell_env.read(), "PATH"),
        Some("/inner:/outer:/base".to_string())
    );

    check_path(outer.path(), Arc::clone(&shell_env)).unwrap();
    assert_eq!(
        system_env(&shell_env.read(), "PATH"),
        Some("/outer:/base".to_string())
    );

    leave(&shell_env, &outside);
    assert_eq!(
        system_env(&shell_env.read(), "PATH"),
        Some("/base".to_string())
    );
}

#[test]
fn duplicate_key_snapshots_previous_only_once() {
    let project = tempfile::tempdir().unwrap();
    write_root_file(&project, ".env", "FOO=one\nFOO=two\n");
    let outside = tempfile::tempdir().unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.unset_system_env_var("FOO");
        env.set_system_env_var("FOO".to_string(), "base".to_string());
        register_root(&mut env, &project);
    }

    enter(&shell_env, &project);
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("two".to_string())
    );
    assert_eq!(root_restore_len(&shell_env.read(), project.path()), 1);

    leave(&shell_env, &outside);
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("base".to_string())
    );
}

#[test]
fn child_cd_inside_active_root_keeps_snapshot() {
    let project = tempfile::tempdir().unwrap();
    write_root_file(&project, ".env", "FOO=project\n");
    let child = project.path().join("subdir");
    fs::create_dir(&child).unwrap();
    let outside = tempfile::tempdir().unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.unset_system_env_var("FOO");
        env.set_system_env_var("FOO".to_string(), "base".to_string());
        register_root(&mut env, &project);
    }

    enter(&shell_env, &project);
    // Moving within the same root must not retake the snapshot.
    check_path(&child, Arc::clone(&shell_env)).unwrap();
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("project".to_string())
    );
    assert!(is_root_active(&shell_env.read(), project.path()));

    leave(&shell_env, &outside);
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("base".to_string())
    );
}

#[test]
fn manual_mutation_while_active_restores_snapshot() {
    let project = tempfile::tempdir().unwrap();
    write_root_file(&project, ".env", "FOO=project\n");
    let outside = tempfile::tempdir().unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.unset_system_env_var("FOO");
        env.set_system_env_var("FOO".to_string(), "base".to_string());
        register_root(&mut env, &project);
    }

    enter(&shell_env, &project);
    shell_env
        .write()
        .set_system_env_var("FOO".to_string(), "user".to_string());

    // The activation snapshot is authoritative: leaving restores the base.
    leave(&shell_env, &outside);
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("base".to_string())
    );
}

#[test]
fn failed_activation_removes_stale_overlay_but_keeps_root() {
    let dir_a = tempfile::tempdir().unwrap();
    write_root_file(&dir_a, ".env", "FOO=A\n");
    let dir_b = tempfile::tempdir().unwrap();
    // Invalid UTF-8 fails `fs::read_to_string` deterministically on any OS.
    fs::write(dir_b.path().join(".env"), [0xff, 0xfe, 0xfd]).unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.unset_system_env_var("FOO");
        env.set_system_env_var("FOO".to_string(), "base".to_string());
        register_root(&mut env, &dir_a);
        register_root(&mut env, &dir_b);
    }

    enter(&shell_env, &dir_a);
    assert_eq!(system_env(&shell_env.read(), "FOO"), Some("A".to_string()));

    let result = check_path(dir_b.path(), Arc::clone(&shell_env));
    assert!(result.is_err());

    // The stale overlay is gone, nothing partial is committed, and the
    // allowed root survives for a later retry.
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("base".to_string())
    );
    assert_eq!(shell_env.read().variable_state.direnv_roots.len(), 2);
    assert!(!is_root_active(&shell_env.read(), dir_a.path()));
    assert!(!is_root_active(&shell_env.read(), dir_b.path()));
}

#[test]
fn nested_failure_keeps_valid_outer_active() {
    let outer = tempfile::tempdir().unwrap();
    write_root_file(&outer, ".env", "FOO=outer\n");
    let inner_path = outer.path().join("sub");
    fs::create_dir(&inner_path).unwrap();
    fs::write(inner_path.join(".env"), [0xff, 0xfe]).unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.unset_system_env_var("FOO");
        env.set_system_env_var("FOO".to_string(), "base".to_string());
        register_root_at(&mut env, &inner_path);
        register_root_at(&mut env, outer.path());
    }

    let result = check_path(&inner_path, Arc::clone(&shell_env));
    assert!(result.is_err());
    assert_eq!(
        system_env(&shell_env.read(), "FOO"),
        Some("outer".to_string())
    );
    assert!(is_root_active(&shell_env.read(), outer.path()));
    assert!(!is_root_active(&shell_env.read(), &inner_path));
    assert_eq!(shell_env.read().variable_state.direnv_roots.len(), 2);
}

#[test]
fn path_derived_lookup_state_follows_activation_and_restore() {
    let project = tempfile::tempdir().unwrap();
    write_root_file(&project, ".env", "PATH=/tmp/custom:/usr/bin\n");
    let outside = tempfile::tempdir().unwrap();

    let shell_env = Environment::new();
    {
        let mut env = shell_env.write();
        env.set_system_env_var("PATH".to_string(), "/usr/bin".to_string());
        register_root(&mut env, &project);
    }

    enter(&shell_env, &project);
    {
        let env = shell_env.read();
        assert_eq!(
            system_env(&env, "PATH"),
            Some("/tmp/custom:/usr/bin".to_string())
        );
        assert_eq!(
            env.variable_state.paths.first().map(String::as_str),
            Some("/tmp/custom")
        );
    }

    leave(&shell_env, &outside);
    {
        let env = shell_env.read();
        assert_eq!(system_env(&env, "PATH"), Some("/usr/bin".to_string()));
        assert_eq!(env.variable_state.paths, vec!["/usr/bin".to_string()]);
    }
}
