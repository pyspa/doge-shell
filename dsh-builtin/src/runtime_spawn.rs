//! Spawning runtime subprocesses from logical shell state.
//!
//! Builtin commands that shell out (`git`, `gh`, editors, hook programs)
//! resolve the executable through the proxy's logical runtime snapshot and
//! spawn with exactly the exported child environment — never through the
//! process-global `PATH` or inherited environment. A logically unset
//! variable stays unset in the child, and an unexported logical `PATH`
//! still resolves.

use crate::ShellProxy;
use anyhow::{Context as _, Result};

/// Build an isolated child [`std::process::Command`] for `program`.
///
/// Resolution, environment isolation, and cwd happen in this one place via
/// the proxy's [`ProcessEnvironmentCapability::command_runtime_snapshot`]:
/// the resolved absolute executable is spawned (never a bare name left for
/// the OS `PATH` search), the child sees exactly the exported environment,
/// and runs in the snapshot cwd. Returns an error when a bare name is not
/// on the logical `PATH`; explicit pathnames are passed toward exec so its
/// diagnostics stay authoritative.
///
/// Callers add their own args (and, rarely, their own `current_dir`
/// override) to the returned builder.
pub fn runtime_command(proxy: &mut dyn ShellProxy, program: &str) -> Result<std::process::Command> {
    let snapshot = proxy.command_runtime_snapshot()?;
    snapshot
        .std_command(program)
        .with_context(|| format!("command not found: {program}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;
    use std::os::unix::fs::PermissionsExt;

    fn write_executable(dir: &std::path::Path, name: &str, exit_code: i32) {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\nexit {exit_code}\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    #[test]
    fn unexported_logical_path_resolves_without_process_path() {
        let logical = tempfile::tempdir().unwrap();
        let process = tempfile::tempdir().unwrap();
        write_executable(logical.path(), "foo", 0);
        write_executable(process.path(), "foo", 3);
        // The process-global PATH is deliberately not touched: the proxy
        // carries PATH A while the environment disagrees.
        let mut proxy = TestShellProxy {
            command_search_paths: vec![logical.path().to_path_buf()],
            ..TestShellProxy::default()
        };
        assert!(!proxy.exported.contains_key("PATH"));
        let status = runtime_command(&mut proxy, "foo")
            .unwrap()
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn missing_bare_program_is_an_error_not_a_spawn() {
        let mut proxy = TestShellProxy::default();
        assert!(runtime_command(&mut proxy, "no-such-tool").is_err());
    }
}
