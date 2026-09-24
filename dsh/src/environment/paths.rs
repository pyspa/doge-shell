//! PATH lookup and command caching.

use super::Environment;
use crate::dirs::search_file;
use std::path::Path;
use tracing::debug;

#[inline]
fn command_contains_slash(cmd: &str) -> bool {
    cmd.contains('/')
}

/// An explicit pathname (`/foo`, `./foo`, `../foo`, `dir/foo`): never a PATH
/// search. The executable bit is deliberately not required here so `execve`
/// stays authoritative for permission diagnostics.
fn explicit_command_file(cmd: &str) -> Option<String> {
    let path = Path::new(cmd);
    (path.exists() && path.is_file()).then(|| cmd.to_string())
}

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

/// A PATH resolution is cacheable only when every entry is absolute.
/// Any relative/empty entry makes the result cwd-dependent.
pub(crate) fn path_lookup_is_cacheable(paths: &[String]) -> bool {
    paths.iter().all(|entry| Path::new(entry).is_absolute())
}

fn lookup_in_paths(paths: &[String], cmd: &str) -> Option<String> {
    paths
        .iter()
        .map(|dir| Path::new(dir).join(cmd))
        .find(|candidate| is_executable_file(candidate))
        .and_then(|path| path.to_str().map(str::to_string))
}

impl Environment {
    /// Expand one PATH entry the way shell word expansion does.
    ///
    /// `~` / `~/...` resolve against the logical shell `HOME` so a
    /// same-session assignment is visible here; without a logical `HOME`
    /// (and for `~user` / `~user/...`) the historical `shellexpand`
    /// system-database lookup applies.
    fn expand_path_entry(&self, input: &str) -> String {
        if (input == "~" || input.starts_with("~/"))
            && let Some(home) = self.lookup_variable("HOME")
        {
            return format!("{home}{}", &input[1..]);
        }

        shellexpand::tilde(input).into_owned()
    }

    /// Insert one entry into the logical `PATH` shell variable.
    ///
    /// The canonical PATH mutation path: the current effective projection
    /// is read only to build the new value, and the write goes through
    /// [`Self::set_shell_var`] so command-location cache invalidation,
    /// `path_generation` bump, and completion re-activation happen through
    /// `refresh_derived_state("PATH")`. Callers must not mutate
    /// `variable_state.paths` directly.
    ///
    /// The existing export attribute is preserved: [`Self::set_shell_var`]
    /// never touches `exported_vars`, so an exported `PATH` stays exported
    /// (and reaches [`Self::child_process_env`]) while an unexported one
    /// stays unexported. No dedup, canonicalization, existence check, or
    /// absolutization is applied: entries are kept as strings.
    ///
    /// An out-of-range index appends at the end instead of panicking:
    /// `ShellProxy::insert_path` accepts an arbitrary index and a panic
    /// here would take the shell down.
    pub(crate) fn insert_path_entry(&mut self, index: usize, path: &str) {
        let path = self.expand_path_entry(path);

        let mut paths = self.variable_state.paths.clone();
        let index = index.min(paths.len());
        paths.insert(index, path);

        self.set_shell_var("PATH".to_string(), paths.join(":"));
    }

    /// Lookup a command in PATH with caching.
    pub fn lookup(&self, cmd: &str) -> Option<String> {
        if command_contains_slash(cmd) {
            return explicit_command_file(cmd);
        }

        let cacheable = path_lookup_is_cacheable(&self.variable_state.paths);

        if cacheable {
            let cached = self.completion_state.command_cache.read().get(cmd).cloned();
            if let Some(cached) = cached {
                if is_executable_file(Path::new(&cached)) {
                    return Some(cached);
                }
                self.completion_state.command_cache.write().remove(cmd);
            }
        }

        let resolved = lookup_in_paths(&self.variable_state.paths, cmd);

        if cacheable && let Some(path) = &resolved {
            self.completion_state
                .command_cache
                .write()
                .insert(cmd.to_string(), path.clone());
        }

        resolved
    }

    /// Lookup with a command-scoped `PATH=...` override.
    ///
    /// Slash-containing names bypass the override as explicit pathnames.
    /// An override search is always fresh: the persistent command cache is
    /// neither read nor populated.
    pub(crate) fn lookup_with_path_override(
        &self,
        cmd: &str,
        path_override: Option<&str>,
    ) -> Option<String> {
        if command_contains_slash(cmd) {
            return explicit_command_file(cmd);
        }
        if let Some(override_value) = path_override {
            let paths: Vec<String> = override_value.split(':').map(|s| s.to_string()).collect();
            return lookup_in_paths(&paths, cmd);
        }
        self.lookup(cmd)
    }

    /// Lookup command with cache update (mutable version for cache population).
    /// Note: With the new interior mutability, this is functionally the same as lookup.
    pub fn lookup_cached(&mut self, cmd: &str) -> Option<String> {
        self.lookup(cmd)
    }

    fn lookup_path_uncached(&self, cmd: &str) -> Option<String> {
        lookup_in_paths(&self.variable_state.paths, cmd)
    }

    /// Search for a command, including fuzzy matching.
    pub fn search(&self, cmd: &str) -> Option<String> {
        if command_contains_slash(cmd) {
            return explicit_command_file(cmd);
        }
        if self.lookup_path_uncached(cmd).is_some() {
            return Some(cmd.to_string());
        }
        for path in &self.variable_state.paths {
            if let Some(file) = search_file(path, cmd) {
                return Some(file);
            }
        }
        None
    }

    /// Reload PATH from the logical shell value.
    ///
    /// `PATH` is shell state even when it is not exported: the shell looks
    /// commands up in it, while `exported_vars` alone decides whether
    /// children see it.
    pub fn reload_path(&mut self) {
        let mut paths: Vec<String> = ["/bin", "/usr/bin", "/sbin", "/usr/sbin"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Resolve `PATH` the way the shell sees it, exported or not:
        // otherwise `PATH=...` moved the shell's own lookup only when
        // exported, and a local assignment left command lookup behind.
        if let Some(val) = self.lookup_variable("PATH") {
            paths = val.split(':').map(|s| s.to_string()).collect();
        }

        self.completion_state.path_generation = self
            .completion_state
            .path_generation
            .checked_add(1)
            .expect("logical PATH generation overflow");

        if paths == self.variable_state.paths {
            // Bulk snapshot restore assigns `variable_state.paths` directly
            // before rebuilding projections. Re-activate even when the value
            // already matches so a worker from the pre-restore PATH generation
            // cannot publish into the restored logical state.
            let _ = crate::completion::generator::activate_system_command_cache(&paths);
            return;
        }

        self.variable_state.paths = paths;
        // Clear command cache when PATH changes.
        self.completion_state.command_cache.write().clear();
        // Activation invalidates the previous generation before the background
        // scan starts; an older worker can no longer publish into the new one.
        self.prewarm_executables();
    }

    /// Reload Z_EXCLUDE from the logical shell value.
    pub fn reload_z_exclude(&mut self) {
        self.variable_state.z_exclude = self
            .lookup_variable("Z_EXCLUDE")
            .map(|val| val.split(':').map(|s| s.to_string()).collect())
            .unwrap_or_default();
    }

    /// Clear the command lookup cache.
    pub fn clear_command_cache(&mut self) {
        self.completion_state.command_cache.get_mut().clear();
    }

    /// Prewarm executable names from a logical PATH snapshot.
    ///
    /// Scan on a worker so callers may hold the Environment write lock while
    /// PATH is being reloaded. Only the shared name vector crosses the thread;
    /// the worker never acquires the Environment lock.
    pub fn prewarm_executables(&self) {
        let paths = self.variable_state.paths.clone();
        let activation = crate::completion::generator::activate_system_command_cache(&paths);
        let scan_ticket =
            crate::completion::generator::begin_background_system_command_scan(&activation);
        let executable_names = std::sync::Arc::clone(&self.completion_state.executable_names);

        // `activate_system_command_cache` cleared the previous generation. The
        // local projection is cleared now as well; a generation-checked worker
        // repopulates only the activation that still owns the cache.
        executable_names.write().clear();

        let worker_ticket = scan_ticket.clone();
        let spawn_result = std::thread::Builder::new()
            .name("dsh-environment-executable-prewarm".to_string())
            .spawn(move || {
                let names = crate::environment::collect_executables(&paths);
                let executable_count = names.len();
                let commands = names.iter().cloned().collect();
                if crate::completion::generator::publish_system_command_scan(
                    &worker_ticket,
                    commands,
                ) && crate::completion::generator::publish_environment_executable_names(
                    worker_ticket.activation(),
                    &executable_names,
                    names,
                ) {
                    debug!("Prewarmed {executable_count} executable names");
                }
            });

        if let Err(error) = spawn_result {
            tracing::warn!("Failed to start executable name prewarm: {error}");
            crate::completion::generator::release_system_command_scan(&scan_ticket);
        }
    }

    /// Set the prewarmed executable names (called after background collection).
    pub fn set_executable_names(&mut self, names: Vec<String>) {
        debug!("Setting {} prewarmed executable names", names.len());
        *self.completion_state.executable_names.write() = names;
    }

    /// Search for an executable name by prefix using the prewarmed cache.
    /// Returns the first matching executable name, or None if not found.
    pub fn search_prefix(&self, prefix: &str) -> Option<String> {
        let names = self.completion_state.executable_names.read();
        if !names.is_empty() {
            // Binary search for the first name >= prefix
            let start = names.partition_point(|name| name.as_str() < prefix);
            if start < names.len() && names[start].starts_with(prefix) {
                return Some(names[start].clone());
            }
            return None;
        }
        drop(names);

        // Do not hold the projection read lock across the synchronous fallback:
        // generation-checked workers publish while holding the global cache read
        // lock, so an I/O-bound holder here would convoy PATH activation.
        self.search(prefix)
    }
}
