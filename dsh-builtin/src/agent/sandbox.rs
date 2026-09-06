//! Optional pinned SRT adapter. Requested isolation never falls back to sh.
use anyhow::{Context, Result, bail};
use dsh_types::agent::TaskGrant;
use serde_json::json;
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

pub const SRT_VERSION: &str = "0.0.75";

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

pub fn find_runtime() -> Result<PathBuf> {
    let paths = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::split_paths(&paths)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("srt"))
        .find(|p| p.is_file())
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
) -> Result<(Command, Option<tempfile::NamedTempFile>)> {
    let (mut command, settings_file) = if grant.sandbox {
        let runtime = find_runtime()?;
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
        let mut command = Command::new("sh");
        command.arg("-c").arg(line);
        (command, None)
    };
    command.current_dir(cwd).env_clear();
    for key in ["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR"]
        .into_iter()
        .chain(grant.environment.iter().map(String::as_str))
    {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    if grant.sandbox {
        // SRT resolves bash from PATH. Prefer the OS shell without granting
        // access to an unrelated package-manager prefix.
        let mut paths: Vec<PathBuf> = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
            .iter()
            .map(PathBuf::from)
            .collect();
        paths.extend(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .filter(|path| path.is_absolute()),
        );
        command.env("PATH", std::env::join_paths(paths)?);
    }
    Ok((command, settings_file))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let line = format!(
            "printf allowed > inside; cat '{}'; printf forbidden > '{}'",
            outside.display(),
            outside.display()
        );
        let (mut cmd, _settings) = command(&line, &inside, &grant, &state).unwrap();
        let result = cmd.output().unwrap();
        assert_eq!(
            std::fs::read_to_string(inside.join("inside")).unwrap_or_default(),
            "allowed",
            "sandbox startup failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!result.status.success());
        assert!(!String::from_utf8_lossy(&result.stdout).contains("secret"));
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "secret");
    }
}
