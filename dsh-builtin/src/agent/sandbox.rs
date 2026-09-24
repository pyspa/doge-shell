//! Optional pinned SRT adapter. Requested isolation never falls back to sh.
use anyhow::{Context, Result, bail};
use dsh_types::agent::TaskGrant;
use serde_json::json;
use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

pub const SRT_VERSION: &str = "0.0.75";

/// Fixed system shell for non-sandboxed agent task execution.
///
/// Not resolved through any PATH: project-controlled `sh` must never become
/// the task execution engine.
const SYSTEM_SHELL: &str = "/bin/sh";

/// Logical shell state snapshot for one persistent agent task spawn.
///
/// Unlike an ordinary interactive child (full exported environment), a task
/// receives only the minimum logical baseline plus explicitly granted
/// `TaskGrant.environment` names. Every value comes from logical shell state;
/// process-global `std::env` is never consulted, so a logically unset
/// variable stays unset in the child.
pub struct SandboxRuntimeSnapshot {
    command_search_paths: Vec<PathBuf>,
    baseline_env: HashMap<String, String>,
    granted_env: HashMap<String, String>,
}

impl SandboxRuntimeSnapshot {
    /// Build a snapshot from logical shell state.
    ///
    /// `command_search_paths` is the logical PATH authority
    /// (`proxy.command_search_paths()`); `get_var` reads logical shell
    /// variables (`proxy.get_var`). `granted_names` is
    /// `TaskGrant.environment`.
    pub fn capture(
        command_search_paths: Vec<PathBuf>,
        get_var: &mut dyn FnMut(&str) -> Option<String>,
        granted_names: &[String],
    ) -> Self {
        let mut baseline_env = HashMap::new();
        if !command_search_paths.is_empty() {
            let joined = command_search_paths
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join(":");
            baseline_env.insert("PATH".to_string(), joined);
        }
        for key in ["HOME", "LANG", "LC_ALL", "TMPDIR"] {
            if let Some(value) = get_var(key) {
                baseline_env.insert(key.to_string(), value);
            }
        }
        let mut granted_env = HashMap::new();
        for name in granted_names {
            if let Some(value) = get_var(name.as_str()) {
                granted_env.insert(name.clone(), value);
            }
        }
        Self {
            command_search_paths,
            baseline_env,
            granted_env,
        }
    }

    /// Convenience wrapper when the caller holds a shell proxy.
    pub fn from_proxy(
        proxy: &mut (impl crate::ShellProxy + ?Sized),
        granted_names: &[String],
    ) -> Self {
        let command_search_paths = proxy.command_search_paths();
        let mut fetch = |key: &str| proxy.get_var(key);
        Self::capture(command_search_paths, &mut fetch, granted_names)
    }

    pub fn command_search_paths(&self) -> &[PathBuf] {
        &self.command_search_paths
    }

    pub fn baseline_env(&self) -> &HashMap<String, String> {
        &self.baseline_env
    }

    pub fn granted_env(&self) -> &HashMap<String, String> {
        &self.granted_env
    }
}

/// Resolve an executable `name` under absolute logical search paths,
/// without any process-global fallback. Single lookup shared by
/// `find_runtime` so the pinned `srt` validation below cannot be bypassed
/// through a second search path.
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };

    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

fn resolve_in_search_paths(search_paths: &[PathBuf], name: &str) -> Option<PathBuf> {
    search_paths
        .iter()
        .filter(|p| p.is_absolute())
        .map(|p| p.join(name))
        .find(|candidate| is_executable_file(candidate))
}

impl std::fmt::Debug for SandboxRuntimeSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print environment values: they may hold secrets.
        f.debug_struct("SandboxRuntimeSnapshot")
            .field("command_search_paths_len", &self.command_search_paths.len())
            .field("baseline_env_len", &self.baseline_env.len())
            .field("granted_env_len", &self.granted_env.len())
            .finish()
    }
}

pub fn settings(grant: &TaskGrant, state_dir: &Path) -> serde_json::Value {
    #[cfg(target_os = "macos")]
    let system_reads = [
        "/usr", "/bin", "/sbin", "/dev", "/etc", "/System", "/Library",
    ];
    #[cfg(not(target_os = "macos"))]
    let system_reads = ["/usr", "/bin", "/sbin", "/dev", "/etc", "/lib", "/lib64"];
    let mut reads: Vec<PathBuf> = system_reads.iter().map(PathBuf::from).collect();
    reads.extend(grant.read_roots.clone());
    reads.extend(grant.write_roots.clone());
    json!({"network":{"allowedDomains":grant.network_hosts,"deniedDomains":[],"allowLocalBinding":false},
        "filesystem":{"denyRead":["/",state_dir],"allowRead":reads,"allowWrite":grant.write_roots,
            "denyWrite":[state_dir]}, "allowPty":false})
}

pub fn find_runtime(search_paths: &[PathBuf]) -> Result<PathBuf> {
    let path = resolve_in_search_paths(search_paths, "srt")
        .context("srt missing; install @anthropic-ai/sandbox-runtime@0.0.75 explicitly")?
        .canonicalize()?;
    let manifest = path
        .parent()
        .and_then(Path::parent)
        .context("invalid srt installation")?
        .join("package.json");
    let package: serde_json::Value = serde_json::from_slice(&std::fs::read(manifest)?)?;
    if package["name"] != "@anthropic-ai/sandbox-runtime" || package["version"] != SRT_VERSION {
        bail!("agent requires @anthropic-ai/sandbox-runtime@{SRT_VERSION}");
    }
    Ok(path)
}

pub fn command(
    line: &str,
    cwd: &Path,
    grant: &TaskGrant,
    state_dir: &Path,
    snapshot: &SandboxRuntimeSnapshot,
) -> Result<(Command, Option<tempfile::NamedTempFile>)> {
    let (mut command, settings_file) = if grant.sandbox {
        let runtime = find_runtime(snapshot.command_search_paths())?;
        let mut file = tempfile::NamedTempFile::new_in(state_dir)?;
        serde_json::to_writer(&mut file, &settings(grant, state_dir))?;
        file.flush()?;
        let mut command = Command::new(runtime);
        command
            .arg("--settings")
            .arg(file.path())
            .arg("-c")
            .arg(line);
        (command, Some(file))
    } else {
        let mut command = Command::new(SYSTEM_SHELL);
        command.arg("-c").arg(line);
        (command, None)
    };
    command.current_dir(cwd).env_clear();
    for (key, value) in snapshot.baseline_env() {
        command.env(key, value);
    }
    if grant.sandbox {
        // SRT resolves bash from PATH. Prefer the OS shell without granting
        // access to an unrelated package-manager prefix. Only logical
        // absolute search paths are added; process-global PATH is never read.
        let mut paths: Vec<PathBuf> = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
            .iter()
            .map(PathBuf::from)
            .collect();
        paths.extend(
            snapshot
                .command_search_paths()
                .iter()
                .filter(|path| path.is_absolute())
                .cloned(),
        );
        command.env("PATH", std::env::join_paths(paths)?);
    }
    // Explicit grants win over both the baseline and the sandbox bootstrap
    // PATH, preserving the existing override semantics.
    for (key, value) in snapshot.granted_env() {
        command.env(key, value);
    }
    Ok((command, settings_file))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{ProcessEnvGuard, TestShellProxy};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::chatgpt::tool::execute::tests::env_lock()
    }

    fn run_task_line(
        line: &str,
        grant: &TaskGrant,
        snapshot: &SandboxRuntimeSnapshot,
        cwd: &Path,
        state_dir: &Path,
    ) -> String {
        let (mut cmd, _settings) = command(line, cwd, grant, state_dir, snapshot).unwrap();
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    #[test]
    fn settings_enforce_explicit_roots_and_network() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let grant = TaskGrant {
            read_roots: vec![root.clone()],
            write_roots: vec![root.clone()],
            network_hosts: vec!["example.com".into()],
            sandbox: true,
            ..Default::default()
        };
        let value = settings(&grant, &root.join("private-state"));
        assert_eq!(
            value["filesystem"]["denyRead"],
            json!(["/", root.join("private-state")])
        );
        assert_eq!(value["network"]["allowedDomains"], json!(["example.com"]));
        assert_eq!(value["filesystem"]["allowWrite"], json!([root]));
        assert_eq!(value["network"]["allowLocalBinding"], false);
    }
    #[test]
    #[ignore = "requires pinned SRT and OS sandbox support; exercised by agent-sandbox CI"]
    fn real_sandbox_blocks_outside_writes_and_reads() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let inside = root.join("workspace");
        std::fs::create_dir(&inside).unwrap();
        let state = root.join("state");
        std::fs::create_dir(&state).unwrap();
        let outside = root.join("outside");
        std::fs::write(&outside, "secret").unwrap();
        let grant = TaskGrant {
            read_roots: vec![inside.clone()],
            write_roots: vec![inside.clone()],
            sandbox: true,
            ..Default::default()
        };
        // Test-only fixture: resolve `srt` the way a logical shell would have
        // imported it at startup. Production `command()` never reads
        // process-global state; this test has no `Environment` to snapshot,
        // so it builds the snapshot input from the runner's PATH directly.
        let search_paths: Vec<PathBuf> =
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
        let snapshot = SandboxRuntimeSnapshot::capture(
            search_paths,
            &mut |key| std::env::var(key).ok(),
            &grant.environment,
        );
        let line = format!(
            "printf allowed > inside; cat '{}'; printf forbidden > '{}'",
            outside.display(),
            outside.display()
        );
        let (mut cmd, _settings) = command(&line, &inside, &grant, &state, &snapshot).unwrap();
        let result = cmd.output().unwrap();
        let stderr = String::from_utf8_lossy(&result.stderr);
        let inside_content = std::fs::read_to_string(inside.join("inside")).unwrap_or_default();
        // GitHub-hosted Ubuntu runners reject bwrap's loopback / namespace
        // setup (e.g. `bwrap: loopback: Failed RTM_NEWADDR: Operation not
        // permitted`). That is an environment constraint, not a sandbox
        // regression, so skip instead of failing CI. The match stays narrow
        // (bwrap startup failure) so a real isolation regression still fails.
        // See .github/workflows/ci.yml `Agent OS sandbox boundary`.
        let sandbox_unavailable = stderr.contains("RTM_NEWADDR")
            || (stderr.contains("bwrap")
                && (stderr.contains("Operation not permitted")
                    || stderr.contains("Permission denied")))
            || (stderr.contains("namespace") && stderr.contains("Operation not permitted"));
        if inside_content != "allowed" && sandbox_unavailable {
            eprintln!("SKIP real_sandbox: OS sandbox unavailable on this runner: {stderr}");
            return;
        }
        assert_eq!(
            inside_content, "allowed",
            "sandbox startup failed: {stderr}"
        );
        assert!(!result.status.success());
        assert!(!String::from_utf8_lossy(&result.stdout).contains("secret"));
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "secret");
    }

    fn proxy_with(vars: &[(&str, &str)], search_paths: Vec<PathBuf>) -> TestShellProxy {
        TestShellProxy {
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            command_search_paths: search_paths,
            ..TestShellProxy::default()
        }
    }

    fn task_setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().to_path_buf();
        let cwd = root.join("work");
        let state = root.join("state");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        (temp, cwd, state)
    }

    #[test]
    fn task_logical_unset_is_never_resurrected_from_process_env() {
        let _lock = env_lock();
        let _stale = ProcessEnvGuard::set("DOGESH_TASK_SECRET", "stale-secret");
        let (_temp, cwd, state) = task_setup();
        let mut proxy = proxy_with(&[], vec![]);
        let grant = TaskGrant {
            environment: vec!["DOGESH_TASK_SECRET".to_string()],
            sandbox: false,
            ..Default::default()
        };
        let snapshot = SandboxRuntimeSnapshot::from_proxy(&mut proxy, &grant.environment);
        let stdout = run_task_line(
            "printf '%s' \"${DOGESH_TASK_SECRET-unset}\"",
            &grant,
            &snapshot,
            &cwd,
            &state,
        );
        assert_eq!(stdout, "unset");
    }

    #[test]
    fn task_explicit_grant_exposes_unexported_logical_variable() {
        let _lock = env_lock();
        let (_temp, cwd, state) = task_setup();
        let mut proxy = proxy_with(&[("DOGESH_TASK_SECRET", "logical-secret")], vec![]);
        assert!(!proxy.exported.contains_key("DOGESH_TASK_SECRET"));
        let grant = TaskGrant {
            environment: vec!["DOGESH_TASK_SECRET".to_string()],
            sandbox: false,
            ..Default::default()
        };
        let snapshot = SandboxRuntimeSnapshot::from_proxy(&mut proxy, &grant.environment);
        let stdout = run_task_line(
            "printf '%s' \"$DOGESH_TASK_SECRET\"",
            &grant,
            &snapshot,
            &cwd,
            &state,
        );
        assert_eq!(stdout, "logical-secret");
    }

    #[test]
    fn task_grant_logical_value_overrides_process_stale_value() {
        let _lock = env_lock();
        let _stale = ProcessEnvGuard::set("DOGESH_TASK_SECRET", "stale");
        let (_temp, cwd, state) = task_setup();
        let mut proxy = proxy_with(&[("DOGESH_TASK_SECRET", "fresh")], vec![]);
        let grant = TaskGrant {
            environment: vec!["DOGESH_TASK_SECRET".to_string()],
            sandbox: false,
            ..Default::default()
        };
        let snapshot = SandboxRuntimeSnapshot::from_proxy(&mut proxy, &grant.environment);
        let stdout = run_task_line(
            "printf '%s' \"$DOGESH_TASK_SECRET\"",
            &grant,
            &snapshot,
            &cwd,
            &state,
        );
        assert_eq!(stdout, "fresh");
    }

    #[test]
    fn task_baseline_home_uses_logical_value() {
        let _lock = env_lock();
        let _process = ProcessEnvGuard::set("HOME", "/stale/process/home");
        let (_temp, cwd, state) = task_setup();
        let mut proxy = proxy_with(&[("HOME", "/logical/home")], vec![]);
        let grant = TaskGrant {
            sandbox: false,
            ..Default::default()
        };
        let snapshot = SandboxRuntimeSnapshot::from_proxy(&mut proxy, &grant.environment);
        let stdout = run_task_line("printf '%s' \"$HOME\"", &grant, &snapshot, &cwd, &state);
        assert_eq!(stdout, "/logical/home");
    }

    #[test]
    fn task_logical_home_unset_does_not_resurrect_process_home() {
        let _lock = env_lock();
        let _process = ProcessEnvGuard::set("HOME", "/stale/process/home");
        let (_temp, cwd, state) = task_setup();
        let mut proxy = proxy_with(&[], vec![]);
        let grant = TaskGrant {
            sandbox: false,
            ..Default::default()
        };
        let snapshot = SandboxRuntimeSnapshot::from_proxy(&mut proxy, &grant.environment);
        let stdout = run_task_line(
            "printf '%s' \"${HOME-unset}\"",
            &grant,
            &snapshot,
            &cwd,
            &state,
        );
        assert_eq!(stdout, "unset");
    }

    #[test]
    fn task_uses_fixed_system_shell() {
        let _lock = env_lock();
        let (_temp, cwd, state) = task_setup();
        let mut proxy = proxy_with(&[], vec![]);
        let grant = TaskGrant {
            sandbox: false,
            ..Default::default()
        };
        let snapshot = SandboxRuntimeSnapshot::from_proxy(&mut proxy, &grant.environment);
        let (cmd, _settings) = command("true", &cwd, &grant, &state, &snapshot).unwrap();
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/bin/sh"));
    }

    fn write_fake_srt_with_mode(root: &Path, mode: u32) -> PathBuf {
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let srt = bin.join("srt");
        std::fs::write(&srt, "#!/bin/sh\necho fake-srt\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&srt).unwrap().permissions();
            perms.set_mode(mode);
            std::fs::set_permissions(&srt, perms).unwrap();
        }
        #[cfg(not(unix))]
        {
            let _ = mode;
        }
        std::fs::write(
            root.join("package.json"),
            serde_json::json!({"name": "@anthropic-ai/sandbox-runtime", "version": SRT_VERSION})
                .to_string(),
        )
        .unwrap();
        bin
    }

    fn write_fake_srt(root: &Path) -> PathBuf {
        write_fake_srt_with_mode(root, 0o755)
    }

    #[test]
    fn find_runtime_uses_logical_search_paths_only() {
        let _lock = env_lock();
        let logical = tempfile::tempdir().unwrap();
        let process = tempfile::tempdir().unwrap();
        let logical_bin = write_fake_srt(logical.path());
        let process_bin = write_fake_srt(process.path());
        let path_value = process_bin.to_string_lossy().to_string();
        let _guard = ProcessEnvGuard::set("PATH", &path_value);
        // Snapshot sees only the logical location even though the process
        // environment points at a different valid runtime.
        let found = find_runtime(std::slice::from_ref(&logical_bin)).unwrap();
        assert_eq!(found, logical_bin.join("srt").canonicalize().unwrap());
    }

    #[test]
    fn find_runtime_does_not_fall_back_to_process_path() {
        let _lock = env_lock();
        let process = tempfile::tempdir().unwrap();
        let process_bin = write_fake_srt(process.path());
        let path_value = process_bin.to_string_lossy().to_string();
        let _guard = ProcessEnvGuard::set("PATH", &path_value);
        let empty: Vec<PathBuf> = Vec::new();
        let err = find_runtime(&empty).expect_err("logical PATH has no srt");
        assert!(err.to_string().contains("srt missing"), "{err}");
    }

    #[test]
    #[cfg(unix)]
    fn find_runtime_skips_non_executable_candidate() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first_bin = write_fake_srt_with_mode(first.path(), 0o644);
        let second_bin = write_fake_srt(second.path());
        let found = find_runtime(&[first_bin, second_bin.clone()]).unwrap();
        assert_eq!(found, second_bin.join("srt").canonicalize().unwrap());
    }

    #[test]
    #[cfg(unix)]
    fn find_runtime_rejects_non_executable_candidate() {
        let logical = tempfile::tempdir().unwrap();
        let logical_bin = write_fake_srt_with_mode(logical.path(), 0o644);
        let err = find_runtime(std::slice::from_ref(&logical_bin))
            .expect_err("non-executable srt must not resolve");
        assert!(err.to_string().contains("srt missing"), "{err}");
    }

    #[test]
    #[cfg(unix)]
    fn find_runtime_accepts_symlink_to_executable() {
        let package = tempfile::tempdir().unwrap();
        let root = package.path().canonicalize().unwrap().to_path_buf();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let real = bin.join("real-srt");
        std::fs::write(&real, "#!/bin/sh\necho fake-srt\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&real).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&real, perms).unwrap();
        }
        std::os::unix::fs::symlink(&real, bin.join("srt")).unwrap();
        std::fs::write(
            root.join("package.json"),
            serde_json::json!({"name": "@anthropic-ai/sandbox-runtime", "version": SRT_VERSION})
                .to_string(),
        )
        .unwrap();
        let found = find_runtime(std::slice::from_ref(&bin)).unwrap();
        assert_eq!(found, real.canonicalize().unwrap());
    }
}
