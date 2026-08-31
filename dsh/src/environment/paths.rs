//! PATH lookup and command caching.

use super::Environment;
use crate::dirs::search_file;
use std::path::Path;
use tracing::debug;

#[inline]
fn is_absolute_command_path(cmd: &str) -> bool {
    cmd.starts_with('/')
}

#[inline]
fn is_relative_command_path(cmd: &str) -> bool {
    cmd.starts_with("./")
}

impl Environment {
    /// Lookup a command in PATH with caching.
    pub fn lookup(&self, cmd: &str) -> Option<String> {
        if is_absolute_command_path(cmd) {
            let cmd_path = Path::new(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return Some(cmd.to_string());
            } else {
                return None;
            }
        }
        if is_relative_command_path(cmd) {
            let cmd_path = Path::new(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return Some(cmd.to_string());
            } else {
                return None;
            }
        }

        // Check cache first for PATH lookups
        {
            if let Some(cached) = self.completion_state.command_cache.read().get(cmd) {
                return cached.clone();
            }
        }

        // Cache miss: search PATH directories
        let result = self.lookup_path_uncached(cmd);

        self.completion_state
            .command_cache
            .write()
            .insert(cmd.to_string(), result.clone());

        result
    }

    /// Lookup command with cache update (mutable version for cache population).
    /// Note: With the new interior mutability, this is functionally the same as lookup.
    pub fn lookup_cached(&mut self, cmd: &str) -> Option<String> {
        self.lookup(cmd)
    }

    fn lookup_path_uncached(&self, cmd: &str) -> Option<String> {
        for path in &self.variable_state.paths {
            let cmd_path = Path::new(path).join(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return cmd_path.to_str().map(|s| s.to_string());
            }
        }
        None
    }

    /// Search for a command, including fuzzy matching.
    pub fn search(&self, cmd: &str) -> Option<String> {
        if is_absolute_command_path(cmd) {
            let cmd_path = Path::new(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return Some(cmd.to_string());
            } else {
                return None;
            }
        }
        if is_relative_command_path(cmd) {
            let cmd_path = Path::new(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return Some(cmd.to_string());
            } else {
                return None;
            }
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

    /// Reload PATH from the environment.
    ///
    /// Called unconditionally at the end of `direnv::check_path`, i.e. on every
    /// `cd`, so it must be cheap when PATH did not actually change. Dropping the
    /// caches when nothing moved is expensive twice over: `command_cache` loses
    /// its memoized lookups, and an empty `executable_names` makes
    /// [`Self::search_prefix`] fall back to a synchronous `read_dir` of every PATH
    /// directory — which runs while the user is typing a command name.
    pub fn reload_path(&mut self) {
        let mut paths: Vec<String> = ["/bin", "/usr/bin", "/sbin", "/usr/sbin"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Resolve `PATH` the way a child would see it, exported shell variable
        // included: otherwise `export PATH=...` moved the children and left the
        // shell's own lookup behind.
        if let Some(val) = self.effective_env_var("PATH").map(str::to_string) {
            paths = val.split(':').map(|s| s.to_string()).collect();
        }

        if paths == self.variable_state.paths {
            return;
        }

        self.variable_state.paths = paths;
        // Clear command cache when PATH changes
        self.completion_state.command_cache.write().clear();
        crate::completion::generator::clear_global_system_commands();
        // Rebuild the executable name cache rather than leaving it empty: an
        // empty cache pushes the cost onto every subsequent keystroke.
        self.prewarm_executables();
    }

    /// Reload Z_EXCLUDE from the environment.
    pub fn reload_z_exclude(&mut self) {
        self.variable_state.z_exclude = self
            .effective_env_var("Z_EXCLUDE")
            .map(|val| val.split(':').map(|s| s.to_string()).collect())
            .unwrap_or_default();
    }

    /// Clear the command lookup cache.
    pub fn clear_command_cache(&mut self) {
        self.completion_state.command_cache.get_mut().clear();
    }

    /// Prewarm the executable names cache by scanning PATH directories.
    /// This should be called in the background after shell startup.
    pub fn prewarm_executables(&self) {
        use std::collections::BTreeSet;
        use std::fs::read_dir;
        use std::os::unix::fs::PermissionsExt;

        let mut names = BTreeSet::new();
        for path in &self.variable_state.paths {
            if let Ok(entries) = read_dir(path) {
                for entry in entries.flatten() {
                    if let Ok(ft) = entry.file_type()
                        && (ft.is_file() || ft.is_symlink())
                        && let Ok(meta) = entry.metadata()
                        && meta.permissions().mode() & 0o111 != 0
                        && let Some(name) = entry.file_name().to_str()
                    {
                        names.insert(name.to_string());
                    }
                }
            }
        }

        let sorted: Vec<String> = names.iter().cloned().collect();
        *self.completion_state.executable_names.write() = sorted;
        crate::completion::generator::set_global_system_commands(names);
        debug!(
            "Prewarmed {} executable names",
            self.completion_state.executable_names.read().len()
        );
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
        if names.is_empty() {
            // Cache not prewarmed yet, fall back to synchronous search
            return self.search(prefix);
        }

        // Binary search for the first name >= prefix
        let start = names.partition_point(|n| n.as_str() < prefix);
        if start < names.len() && names[start].starts_with(prefix) {
            return Some(names[start].clone());
        }
        None
    }
}
