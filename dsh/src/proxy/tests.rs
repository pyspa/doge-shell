//! Tests for `proxy/mod.rs`'s own free functions: direnv root matching and the y/N confirmation prompt.
use super::*;
use crate::environment::Environment;
use crate::shell::Shell;
use dsh_builtin::ShellProxy;
use std::fs;

#[test]
fn direnv_allowance_requires_exact_root_match() {
    let dir = tempfile::tempdir().unwrap();
    let allowed = dir.path().join("repo");
    let child = allowed.join("subproject");
    fs::create_dir_all(&child).unwrap();

    assert!(is_same_direnv_root(&allowed, &allowed));
    assert!(
        !is_same_direnv_root(&child, &allowed),
        "allow-direnv should not implicitly trust nested project roots"
    );
}

#[test]
fn confirmation_accepts_only_single_y() {
    assert!(confirmation_is_yes("y\n"));
    assert!(confirmation_is_yes("Y\r\n"));
    assert!(confirmation_is_yes(" y "));

    assert!(!confirmation_is_yes(""));
    assert!(!confirmation_is_yes("\n"));
    assert!(!confirmation_is_yes("n\n"));
    assert!(!confirmation_is_yes("yes\n"));
    assert!(!confirmation_is_yes("1\n"));
}

#[test]
fn confirmation_reads_tty_only_when_stdin_is_terminal() {
    assert!(should_read_confirmation_from_tty(true));
    assert!(!should_read_confirmation_from_tty(false));
}

#[test]
fn capture_command_uses_logical_child_environment() {
    let _lock = crate::test_env_lock();
    let environment = Environment::new();
    // Plant process-only state after `Environment::new()` so startup import
    // cannot explain what the child sees.
    let _process_only = crate::ProcessEnvGuard::set("DOGESH_CAPTURE_PROCESS_ONLY", "stale-secret");
    let _stale_visible = crate::ProcessEnvGuard::set("DOGESH_CAPTURE_VISIBLE", "stale-process");
    {
        let mut env = environment.write();
        env.set_and_export_shell_var("DOGESH_CAPTURE_VISIBLE".to_string(), "logical".to_string());
    }
    let mut shell = Shell::new(environment);
    let pid = nix::unistd::getpid();
    let ctx = Context::new_safe(pid, pid, true);
    let (code, stdout, _) = shell
        .capture_command(
            &ctx,
            "printf '%s|%s' \"$DOGESH_CAPTURE_VISIBLE\" \"${DOGESH_CAPTURE_PROCESS_ONLY-unset}\"",
        )
        .expect("capture runs");
    assert_eq!(code, 0);
    assert_eq!(stdout, "logical|unset");
}
