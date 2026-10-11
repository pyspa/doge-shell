//! Real owned child processes, with no external service or developer terminal.
use super::{command, command_result};
use crate::environment::Environment;
use crate::lisp::{Env, Value, make_env};
use crate::safety::SafetyLevel;
use std::os::unix::fs::PermissionsExt;
use std::{cell::RefCell, rc::Rc};

fn string(value: &str) -> Value {
    Value::String(value.into())
}
fn field(result: &Value, key: &str) -> Value {
    let Value::HashMap(map) = result else {
        panic!("expected result hash")
    };
    assert_eq!(map.borrow().len(), 4);
    map.borrow().get(&string(key)).unwrap().clone()
}

fn probe(body: &str) -> (tempfile::TempDir, String, Rc<RefCell<Env>>) {
    let dir = tempfile::tempdir().unwrap();
    let sh = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("sh"))
        .find(|path| path.is_file())
        .unwrap();
    let path = dir.path().join("probe");
    std::fs::write(&path, format!("#!{}\n{body}\n", sh.display())).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    (
        dir,
        path.to_str().unwrap().into(),
        make_env(Environment::new()),
    )
}

#[test]
fn status_and_both_streams_preserve_whitespace() {
    let _guard = crate::test_env_lock();
    let (_dir, path, env) = probe("printf 'out \\n\\n'; printf 'err\\t\\n' >&2; exit 7");
    let result = command_result(env.clone(), vec![string(&path)]).unwrap();
    assert_eq!(field(&result, "exit-code"), Value::Int(7));
    assert_eq!(field(&result, "signal"), Value::NIL);
    assert_eq!(field(&result, "stdout"), string("out \n\n"));
    assert_eq!(field(&result, "stderr"), string("err\t\n"));
    // The old API still chooses stdout and trims it, including nonzero exits.
    assert_eq!(command(env, vec![string(&path)]).unwrap(), string("out"));
}

#[test]
fn stderr_only_and_empty_success_are_unambiguous() {
    let _guard = crate::test_env_lock();
    for (body, code, stderr) in [
        ("exit 0", 0, ""),
        ("printf 'err \\n' >&2; exit 3", 3, "err \n"),
    ] {
        let (_dir, path, env) = probe(body);
        let result = command_result(env.clone(), vec![string(&path)]).unwrap();
        assert_eq!(field(&result, "exit-code"), Value::Int(code));
        assert_eq!(field(&result, "stdout"), string(""));
        assert_eq!(field(&result, "stderr"), string(stderr));
        assert_eq!(
            command(env, vec![string(&path)]).unwrap(),
            string(stderr.trim_end())
        );
    }
}

#[test]
fn signal_termination_is_not_a_synthetic_exit_code() {
    let _guard = crate::test_env_lock();
    let (_dir, path, env) = probe("printf before; printf err >&2; kill -INT $$");
    let result = command_result(env, vec![string(&path)]).unwrap();
    assert_eq!(field(&result, "exit-code"), Value::NIL);
    assert_eq!(field(&result, "signal"), Value::Int(libc::SIGINT.into()));
    assert_eq!(field(&result, "stdout"), string("before"));
    assert_eq!(field(&result, "stderr"), string("err"));
}

#[test]
fn literal_arguments_and_utf8_replacement_are_explicit() {
    let _guard = crate::test_env_lock();
    let (_dir, path, env) = probe("printf '%s\\n' \"$@\"; printf '\\377' >&2");
    let args = [
        "日本語",
        "a b",
        "",
        "a'b\"c",
        "*.rs",
        ";touch marker",
        "$(echo x)",
        "line1\nline2",
    ];
    let mut values = vec![string(&path)];
    values.extend(args.iter().map(|v| string(v)));
    let result = command_result(env, values).unwrap();
    assert_eq!(field(&result, "stdout"), string(&(args.join("\n") + "\n")));
    assert_eq!(field(&result, "stderr"), string("\u{fffd}"));
}

#[test]
fn invalid_arguments_and_spawn_errors_never_fabricate_a_result() {
    let _guard = crate::test_env_lock();
    let (dir, path, env) = probe("printf ran > \"$1\"");
    let marker = dir.path().join("marker");
    for args in [
        vec![],
        vec![Value::Int(1)],
        vec![string("")],
        vec![string("a\0b")],
        vec![
            string(&path),
            string(marker.to_str().unwrap()),
            Value::Int(1),
        ],
    ] {
        assert!(command_result(env.clone(), args).is_err());
    }
    assert!(!marker.exists());
    assert!(command_result(env.clone(), vec![string("dogesh-missing-result-xyz")]).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(command_result(env, vec![string(&path)]).is_err());
}

#[test]
fn safety_and_logical_runtime_are_shared_with_command() {
    let _guard = crate::test_env_lock();
    let (dir, path, _) =
        probe("printf '%s' \"${DOGESH_RESULT_PRIVATE-unset}\"; printf '%s' \"$PWD\" >&2");
    let _stale = crate::ProcessEnvGuard::set("DOGESH_RESULT_PRIVATE", "stale");
    let shell = Environment::new();
    {
        let mut s = shell.write();
        s.unset_shell_var("DOGESH_RESULT_PRIVATE");
        s.unset_shell_var("PATH");
        s.set_shell_var("PATH".into(), dir.path().to_str().unwrap().into());
    }
    let env = make_env(shell.clone());
    let result = command_result(env.clone(), vec![string("probe")]).unwrap();
    assert_eq!(field(&result, "stdout"), string("unset"));
    assert_eq!(
        field(&result, "stderr"),
        string(std::env::current_dir().unwrap().to_str().unwrap())
    );
    *shell.read().policy_state.safety_level.write() = SafetyLevel::Strict;
    let marker = dir.path().join("marker");
    assert!(
        command_result(
            env.clone(),
            vec![string(&path), string(marker.to_str().unwrap())]
        )
        .unwrap_err()
        .msg
        .contains("SafetyGuard Blocked")
    );
    assert!(
        command(env, vec![string(&path)])
            .unwrap_err()
            .msg
            .contains("SafetyGuard Blocked")
    );
    assert!(!marker.exists());
}
