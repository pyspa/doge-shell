//! Immutable runtime snapshot for spawning child processes.
//!
//! [`CommandRuntimeSnapshot`] is the single authority that turns a shell
//! command name into a spawned child: logical command search paths resolve
//! the executable, the exported child environment is passed verbatim, and
//! the snapshot cwd anchors relative `PATH` entries. It deliberately holds
//! only these three facts — no shell variables, aliases, safety policy, or
//! caches — so every runtime consumer (external dispatch, prompt probes,
//! Lisp `(command ...)`, editors, doctor) shares one resolution semantics.
//!
//! Logical `PATH` and child `PATH` stay distinct: an unexported logical
//! `PATH` still resolves executables here, while the child environment only
//! carries what the shell exported. Process-global `std::env` is never
//! consulted, so a logically unset variable stays unset.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The three runtime facts a child spawn may depend on.
#[derive(Debug, Clone)]
pub struct CommandRuntimeSnapshot {
    command_search_paths: Vec<PathBuf>,
    child_env: Arc<HashMap<String, String>>,
    current_dir: PathBuf,
}

impl CommandRuntimeSnapshot {
    /// Build a snapshot from already-materialized runtime state.
    ///
    /// Callers pass the logical command search paths
    /// (`Environment.variable_state.paths`), the exported child environment
    /// (`Environment::child_process_env`), and the cwd to resolve relative
    /// entries against. Nothing is read from the process environment here.
    pub fn new(
        command_search_paths: Vec<PathBuf>,
        child_env: HashMap<String, String>,
        current_dir: PathBuf,
    ) -> Self {
        Self {
            command_search_paths,
            child_env: Arc::new(child_env),
            current_dir,
        }
    }

    /// The cwd relative `PATH` entries and explicit relative pathnames
    /// resolve against.
    pub fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    /// The exported child environment. Passed verbatim to the child; a
    /// logically unset variable is absent here even when the process
    /// environment still holds a stale value.
    pub fn child_env(&self) -> &HashMap<String, String> {
        &self.child_env
    }

    /// Cheap shared handle to the child environment for value-compared
    /// runtime identities.
    pub fn child_env_shared(&self) -> Arc<HashMap<String, String>> {
        Arc::clone(&self.child_env)
    }

    /// The logical command search paths, in lookup order.
    pub fn command_search_paths(&self) -> &[PathBuf] {
        &self.command_search_paths
    }

    /// Resolve a bare program name against the logical `PATH`, in order.
    ///
    /// A name containing `/` is never a `PATH` search and yields `None`
    /// (see [`Self::resolve_program`]). Absolute entries are used as-is,
    /// relative entries resolve against the snapshot cwd, and an empty
    /// entry behaves like the shell's cwd fallback. Only regular files
    /// with at least one execute bit match; symlinks are followed via
    /// `metadata`. `PATH` order is preserved.
    pub fn resolve_bare_program(&self, name: &str) -> Option<PathBuf> {
        if name.is_empty() || name.contains('/') {
            return None;
        }
        for entry in &self.command_search_paths {
            let base = if entry.as_os_str().is_empty() {
                self.current_dir.clone()
            } else if entry.is_absolute() {
                entry.clone()
            } else {
                self.current_dir.join(entry)
            };
            let candidate = base.join(name);
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// Resolve `name` to the executable path to spawn.
    ///
    /// Bare names go through [`Self::resolve_bare_program`]. A name
    /// containing `/` bypasses `PATH` search: absolute pathnames are used
    /// as-is, relative ones join onto the snapshot cwd. Explicit pathnames
    /// are never rejected for missing files or permission bits here — the
    /// actual spawn reports `ENOENT`/`EACCES`, keeping exec diagnostics
    /// authoritative like `Environment::lookup`.
    pub fn resolve_program(&self, name: &str) -> Option<PathBuf> {
        if name.is_empty() {
            return None;
        }
        if !name.contains('/') {
            return self.resolve_bare_program(name);
        }
        let path = Path::new(name);
        if path.is_absolute() {
            Some(path.to_path_buf())
        } else {
            Some(self.current_dir.join(path))
        }
    }

    /// Build a child [`std::process::Command`] for `program`.
    ///
    /// Resolution, environment isolation, and cwd happen in this one place:
    /// the resolved executable path is spawned (never a bare name left for
    /// the OS `PATH` search, so an unexported logical `PATH` still works),
    /// the child sees exactly the snapshot environment via `env_clear` +
    /// `envs`, and runs in the snapshot cwd. Returns `None` when a bare
    /// name is not on the logical `PATH`.
    ///
    /// On Unix the original program name is kept as `argv[0]` via
    /// `CommandExt::arg0`, so resolving `foo` to `/custom/bin/foo` does not
    /// change what the child sees as its own name.
    pub fn std_command(&self, program: &str) -> Option<std::process::Command> {
        let executable = self.resolve_program(program)?;
        let mut command = std::process::Command::new(&executable);
        command
            .env_clear()
            .envs(self.child_env.iter())
            .current_dir(&self.current_dir);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.arg0(program);
        }
        Some(command)
    }
}

/// Same executable predicate as shell/task/project resolution: a regular
/// file with any execute bit. `metadata` follows symlinks so a linked
/// binary stays usable.
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write_executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn write_plain_file(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "not executable\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn snapshot_for(paths: Vec<PathBuf>, cwd: &Path) -> CommandRuntimeSnapshot {
        CommandRuntimeSnapshot::new(paths, HashMap::new(), cwd.to_path_buf())
    }

    #[test]
    fn path_order_wins() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let expected = write_executable(&a, "foo");
        write_executable(&b, "foo");
        let snapshot = snapshot_for(vec![a, b], root.path());
        assert_eq!(snapshot.resolve_bare_program("foo"), Some(expected));
    }

    #[test]
    fn non_executable_is_skipped() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        write_plain_file(&a, "foo");
        let expected = write_executable(&b, "foo");
        let snapshot = snapshot_for(vec![a, b], root.path());
        assert_eq!(snapshot.resolve_bare_program("foo"), Some(expected));
    }

    #[test]
    fn relative_path_entry_resolves_against_snapshot_cwd() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let expected = write_executable(&bin, "foo");
        let snapshot = snapshot_for(vec![PathBuf::from("bin")], root.path());
        assert_eq!(snapshot.resolve_bare_program("foo"), Some(expected));
    }

    #[test]
    fn empty_path_entry_falls_back_to_snapshot_cwd() {
        let root = tempfile::tempdir().unwrap();
        let expected = write_executable(root.path(), "foo");
        let snapshot = snapshot_for(vec![PathBuf::from("")], root.path());
        assert_eq!(snapshot.resolve_bare_program("foo"), Some(expected));
    }

    #[test]
    fn slash_name_bypasses_path_search() {
        let root = tempfile::tempdir().unwrap();
        let other = root.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        write_executable(&other, "foo");
        let sub = root.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        write_executable(&sub, "foo");
        // Bare lookup would find other/foo; the slash form must not.
        let snapshot = snapshot_for(vec![other], root.path());
        assert_eq!(snapshot.resolve_bare_program("sub/foo"), None);
        assert_eq!(
            snapshot.resolve_program("sub/foo"),
            Some(root.path().join("sub/foo"))
        );
    }

    #[test]
    fn explicit_missing_path_stays_a_spawn_candidate() {
        let root = tempfile::tempdir().unwrap();
        let snapshot = snapshot_for(vec![], root.path());
        // Not rejected as command-not-found: the spawn reports ENOENT.
        assert_eq!(
            snapshot.resolve_program("sub/missing"),
            Some(root.path().join("sub/missing"))
        );
        assert!(snapshot.std_command("sub/missing").is_some());
        assert!(snapshot.std_command("no-such-bare-command").is_none());
    }

    #[test]
    fn executable_symlink_resolves() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let real = write_executable(&bin, "real");
        std::os::unix::fs::symlink(&real, bin.join("foo")).unwrap();
        let snapshot = snapshot_for(vec![bin.clone()], root.path());
        assert_eq!(snapshot.resolve_bare_program("foo"), Some(bin.join("foo")));
    }

    #[test]
    fn std_command_isolates_environment_and_cwd() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_executable(&bin, "foo");
        let mut child_env = HashMap::new();
        child_env.insert("FOO".to_string(), "logical".to_string());
        let snapshot =
            CommandRuntimeSnapshot::new(vec![bin.clone()], child_env, root.path().to_path_buf());
        let command = snapshot.std_command("foo").unwrap();
        assert_eq!(command.get_program(), bin.join("foo").as_os_str());
        let vars: HashMap<_, _> = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(vars.get("FOO").and_then(|v| v.as_deref()), Some("logical"));
        assert_eq!(command.get_current_dir(), Some(root.path()));
    }
}
