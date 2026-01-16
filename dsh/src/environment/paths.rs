//! PATH lookup and command caching.

use super::{ABSOLUTE_PATH_REGEX, Environment, RELATIVE_PATH_REGEX};
use crate::dirs::search_file;
use std::collections::HashSet;
use std::env;
use std::path::Path;
use tracing::debug;

impl Environment {
    /// Lookup a command in PATH with caching.
    pub fn lookup(&self, cmd: &str) -> Option<String> {
        if ABSOLUTE_PATH_REGEX.is_match(cmd) {
            let cmd_path = Path::new(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return Some(cmd.to_string());
            } else {
                return None;
            }
        }
        if RELATIVE_PATH_REGEX.is_match(cmd) {
            let cmd_path = Path::new(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return Some(cmd.to_string());
            } else {
                return None;
            }
        }

        // Check cache first for PATH lookups
        {
            if let Some(cached) = self.command_cache.read().get(cmd) {
                return cached.clone();
            }
        }

        // Cache miss: search PATH directories
        let result = self.lookup_path_uncached(cmd);

        // Update cache
        self.command_cache
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
        for path in &self.paths {
            let cmd_path = Path::new(path).join(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return cmd_path.to_str().map(|s| s.to_string());
            }
        }
        None
    }

    /// Search for a command, including fuzzy matching.
    pub fn search(&self, cmd: &str) -> Option<String> {
        if ABSOLUTE_PATH_REGEX.is_match(cmd) {
            let cmd_path = Path::new(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return Some(cmd.to_string());
            } else {
                return None;
            }
        }
        if RELATIVE_PATH_REGEX.is_match(cmd) {
            let cmd_path = Path::new(cmd);
            if cmd_path.exists() && cmd_path.is_file() {
                return Some(cmd.to_string());
            } else {
                return None;
            }
        }
        for path in &self.paths {
            if let Some(file) = search_file(path, cmd) {
                return Some(file);
            }
        }
        None
    }

    /// Reload PATH from the environment.
    pub fn reload_path(&mut self) {
        let mut paths: Vec<String> = ["/bin", "/usr/bin", "/sbin", "/usr/sbin"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        if let Ok(val) = env::var("PATH") {
            paths = val.split(':').map(|s| s.to_string()).collect();
        }
        self.paths = paths;
        // Clear command cache when PATH changes
        self.command_cache.write().clear();
        // Also clear executable names cache
        self.executable_names.write().clear();
        crate::completion::generator::clear_global_system_commands();
    }

    /// Reload Z_EXCLUDE from the environment.
    pub fn reload_z_exclude(&mut self) {
        self.z_exclude = super::parse_z_exclude();
    }

    /// Clear the command lookup cache.
    pub fn clear_command_cache(&mut self) {
        self.command_cache.get_mut().clear();
    }

    /// Prewarm the executable names cache by scanning PATH directories.
    /// This should be called in the background after shell startup.
    pub fn prewarm_executables(&self) {
        use std::fs::read_dir;
        use std::os::unix::fs::PermissionsExt;

        let mut names = HashSet::new();
        for path in &self.paths {
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

        let mut sorted: Vec<String> = names.iter().cloned().collect();
        sorted.sort();
        *self.executable_names.write() = sorted;
        crate::completion::generator::set_global_system_commands(names);
        debug!(
            "Prewarmed {} executable names",
            self.executable_names.read().len()
        );
    }

    /// Set the prewarmed executable names (called after background collection).
    pub fn set_executable_names(&mut self, names: Vec<String>) {
        debug!("Setting {} prewarmed executable names", names.len());
        *self.executable_names.write() = names;
    }

    /// Search for an executable name by prefix using the prewarmed cache.
    /// Returns the first matching executable name, or None if not found.
    pub fn search_prefix(&self, prefix: &str) -> Option<String> {
        let names = self.executable_names.read();
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
