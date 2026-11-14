use super::super::*;
use std::os::fd::FromRawFd as _;

#[test]
fn request_rejects_unknown_version() {
    let env_arc = crate::environment::Environment::new();
    let snapshot = ChildShellSnapshot::capture(&env_arc.read());
    let request = InternalExecRequest {
        version: PROTOCOL_VERSION + 1,
        snapshot,
        kind: InternalExecKind::Builtin(BuiltinExecRequest {
            name: "echo".to_string(),
            argv: vec!["echo".to_string()],
            env_overrides: vec![],
        }),
    };
    let bytes = serde_json::to_vec(&request).expect("encode");
    // Feed through a real pipe so the size-cap and EOF logic run.
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    let read_fd = read.into_raw_fd();
    // `read_internal_request` owns and closes the fd; no second close.
    let err = read_internal_request(read_fd).expect_err("unknown version must fail");
    assert!(
        err.to_string()
            .contains("unsupported internal exec version")
    );
}

#[test]
fn request_rejects_legacy_v5() {
    // The v5 schema (`ChildShellSnapshot` with `system_env_vars`) must
    // fail closed after the v6 unification, never parse as a v6 request.
    let env_arc = crate::environment::Environment::new();
    let snapshot = ChildShellSnapshot::capture(&env_arc.read());
    let request = InternalExecRequest {
        version: 5,
        snapshot,
        kind: InternalExecKind::Builtin(BuiltinExecRequest {
            name: "echo".to_string(),
            argv: vec!["echo".to_string()],
            env_overrides: vec![],
        }),
    };
    let bytes = serde_json::to_vec(&request).expect("encode");
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    let read_fd = read.into_raw_fd();
    // Owned (and closed) by `read_internal_request`.
    let err = read_internal_request(read_fd).expect_err("legacy v5 must fail");
    assert!(
        err.to_string()
            .contains("unsupported internal exec version")
    );
}

#[test]
fn request_rejects_legacy_v3() {
    // The v3 schema (plan requests without `signal_policy`) must fail
    // closed after the v4 migration, never parse as a v4 request.
    let env_arc = crate::environment::Environment::new();
    let snapshot = ChildShellSnapshot::capture(&env_arc.read());
    let request = InternalExecRequest {
        version: 3,
        snapshot,
        kind: InternalExecKind::Builtin(BuiltinExecRequest {
            name: "echo".to_string(),
            argv: vec!["echo".to_string()],
            env_overrides: vec![],
        }),
    };
    let bytes = serde_json::to_vec(&request).expect("encode");
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    let read_fd = read.into_raw_fd();
    // Owned (and closed) by `read_internal_request`.
    let err = read_internal_request(read_fd).expect_err("legacy v3 must fail");
    assert!(
        err.to_string()
            .contains("unsupported internal exec version")
    );
}

#[test]
fn request_rejects_legacy_v2() {
    // The v2 schema (no `NoCommand` helper kind) must fail closed
    // after the v3 migration, never parse as a v3 request.
    let env_arc = crate::environment::Environment::new();
    let snapshot = ChildShellSnapshot::capture(&env_arc.read());
    let request = InternalExecRequest {
        version: 2,
        snapshot,
        kind: InternalExecKind::Builtin(BuiltinExecRequest {
            name: "echo".to_string(),
            argv: vec!["echo".to_string()],
            env_overrides: vec![],
        }),
    };
    let bytes = serde_json::to_vec(&request).expect("encode");
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    let read_fd = read.into_raw_fd();
    // Owned (and closed) by `read_internal_request`.
    let err = read_internal_request(read_fd).expect_err("legacy v2 must fail");
    assert!(
        err.to_string()
            .contains("unsupported internal exec version")
    );
}

#[test]
fn request_rejects_legacy_v1() {
    // The v1 schema (flat `jobs` plan, no `AsyncAndOrList` mode) must
    // fail closed after the v2 migration, never parse as a v2 request.
    let env_arc = crate::environment::Environment::new();
    let snapshot = ChildShellSnapshot::capture(&env_arc.read());
    let request = InternalExecRequest {
        version: 1,
        snapshot,
        kind: InternalExecKind::Builtin(BuiltinExecRequest {
            name: "echo".to_string(),
            argv: vec!["echo".to_string()],
            env_overrides: vec![],
        }),
    };
    let bytes = serde_json::to_vec(&request).expect("encode");
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    let read_fd = read.into_raw_fd();
    // Owned (and closed) by `read_internal_request`.
    let err = read_internal_request(read_fd).expect_err("legacy v1 must fail");
    assert!(
        err.to_string()
            .contains("unsupported internal exec version")
    );
}

#[test]
fn request_rejects_garbage() {
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(b"not json").expect("write");
    drop(write);
    let read_fd = read.into_raw_fd();
    // Owned (and closed) by `read_internal_request`.
    assert!(read_internal_request(read_fd).is_err());
}

#[test]
fn request_rejects_legacy_v6() {
    // v6 payloads (old `PlannedSubstitution.kind = "ProcessSubstitution"`)
    // must fail closed after the v7 direction migration, never parse as
    // `<(...)`.
    let env_arc = crate::environment::Environment::new();
    let snapshot = ChildShellSnapshot::capture(&env_arc.read());
    let request = InternalExecRequest {
        version: 6,
        snapshot,
        kind: InternalExecKind::Builtin(BuiltinExecRequest {
            name: "echo".to_string(),
            argv: vec!["echo".to_string()],
            env_overrides: vec![],
        }),
    };
    let bytes = serde_json::to_vec(&request).expect("encode");
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    let read_fd = read.into_raw_fd();
    let err = read_internal_request(read_fd).expect_err("legacy v6 must fail");
    assert!(
        err.to_string()
            .contains("unsupported internal exec version")
    );
}

#[test]
fn request_rejects_legacy_v7() {
    // v7 payloads (no structured `ParameterExpansion`) must fail closed
    // after the v8 migration, never parse as a v8 request.
    let env_arc = crate::environment::Environment::new();
    let snapshot = ChildShellSnapshot::capture(&env_arc.read());
    let request = InternalExecRequest {
        version: 7,
        snapshot,
        kind: InternalExecKind::Builtin(BuiltinExecRequest {
            name: "echo".to_string(),
            argv: vec!["echo".to_string()],
            env_overrides: vec![],
        }),
    };
    let bytes = serde_json::to_vec(&request).expect("encode");
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    let read_fd = read.into_raw_fd();
    let err = read_internal_request(read_fd).expect_err("legacy v7 must fail");
    assert!(
        err.to_string()
            .contains("unsupported internal exec version")
    );
}

#[test]
fn request_rejects_legacy_v8() {
    // v8 payloads (no structured `ArithmeticExpansion`) must fail closed
    // after the v9 migration, never parse as a v9 request.
    let env_arc = crate::environment::Environment::new();
    let snapshot = ChildShellSnapshot::capture(&env_arc.read());
    let request = InternalExecRequest {
        version: 8,
        snapshot,
        kind: InternalExecKind::Builtin(BuiltinExecRequest {
            name: "echo".to_string(),
            argv: vec!["echo".to_string()],
            env_overrides: vec![],
        }),
    };
    let bytes = serde_json::to_vec(&request).expect("encode");
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    let read_fd = read.into_raw_fd();
    let err = read_internal_request(read_fd).expect_err("legacy v8 must fail");
    assert!(
        err.to_string()
            .contains("unsupported internal exec version")
    );
}

#[test]
fn request_rejects_legacy_v9() {
    // v9 payloads have no invocation state/WordPart schema; reject them
    // before interpreting helper execution data.
    let env_arc = crate::environment::Environment::new();
    let snapshot = ChildShellSnapshot::capture(&env_arc.read());
    let request = InternalExecRequest {
        version: 9,
        snapshot,
        kind: InternalExecKind::Builtin(BuiltinExecRequest {
            name: "echo".to_string(),
            argv: vec!["echo".to_string()],
            env_overrides: vec![],
        }),
    };
    let bytes = serde_json::to_vec(&request).expect("encode");
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    let read_fd = read.into_raw_fd();
    let err = read_internal_request(read_fd).expect_err("legacy v9 must fail");
    assert!(
        err.to_string()
            .contains("unsupported internal exec version")
    );
}
