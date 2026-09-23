//! Directory-scoped environment overlays (`direnv`).
//!
//! Each allowed root is policy (a trusted path), never restoration state.
//! Activation snapshots the values it is about to replace into exactly one
//! reversible overlay per root; leaving restores that snapshot. Transitions
//! always unload deepest-first, then load shallowest-first.

use crate::environment::Environment;
use crate::environment::variables::ShellVarState;
use anyhow::Result;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum Entry {
    Env(EnvEntry),
    PathAdd(PathAddEntry),
}

#[derive(Debug, Clone)]
pub struct PathAddEntry {
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
}

/// Previous logical state captured at activation time for one overlaid key.
#[derive(Debug, Clone)]
struct EnvRestore {
    key: String,
    previous: ShellVarState,
}

/// Runtime activation state owned by exactly one active root.
#[derive(Debug, Clone)]
struct ActiveDirEnvironment {
    /// First-touch order; deactivation unwinds it in reverse.
    restore: Vec<EnvRestore>,
}

#[derive(Debug, Clone)]
pub struct DirEnvironment {
    pub path: String,
    active: Option<ActiveDirEnvironment>,
}

/// Presentation events; `check_path` renders them after reconciling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirenvEvent {
    Loaded { path: String, exported: Vec<String> },
    Unloaded { path: String },
}

/// A fully materialized activation: what to restore, and what to commit.
/// Built before any environment mutation so a failure commits nothing.
struct DirEnvPatch {
    restore: Vec<EnvRestore>,
    /// Final value per touched key, in first-touch order.
    values: Vec<(String, String)>,
}

struct PatchBuilder<'a> {
    env: &'a Environment,
    seen: HashSet<String>,
    restore: Vec<EnvRestore>,
    values: Vec<(String, String)>,
    index: HashMap<String, usize>,
}

impl<'a> PatchBuilder<'a> {
    fn new(env: &'a Environment) -> Self {
        Self {
            env,
            seen: HashSet::new(),
            restore: Vec::new(),
            values: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Snapshot the pre-activation logical state of `key` exactly once.
    fn touch(&mut self, key: &str) {
        if self.seen.insert(key.to_string()) {
            self.restore.push(EnvRestore {
                key: key.to_string(),
                previous: self.env.shell_var_state(key),
            });
        }
    }

    fn upsert(&mut self, key: String, value: String) {
        if let Some(&i) = self.index.get(&key) {
            self.values[i].1 = value;
        } else {
            self.index.insert(key.clone(), self.values.len());
            self.values.push((key, value));
        }
    }

    /// Current working value: earlier entries in this patch win over the
    /// pre-activation baseline (the logical shell value).
    fn working(&self, key: &str) -> Option<String> {
        self.index
            .get(key)
            .map(|&i| self.values[i].1.clone())
            .or_else(|| self.env.lookup_variable(key))
    }

    fn finish(self) -> DirEnvPatch {
        DirEnvPatch {
            restore: self.restore,
            values: self.values,
        }
    }
}

fn build_patch(entries: &[Entry], env: &Environment) -> DirEnvPatch {
    let mut builder = PatchBuilder::new(env);
    for entry in entries {
        match entry {
            Entry::Env(env_entry) => {
                builder.touch(&env_entry.key);
                builder.upsert(env_entry.key.clone(), env_entry.value.clone());
            }
            Entry::PathAdd(path_entry) => {
                // PATH_ADD is a PATH mutation: it must snapshot PATH too.
                builder.touch("PATH");
                let current = builder.working("PATH").unwrap_or_default();
                builder.upsert(
                    "PATH".to_string(),
                    prepend_path_entry(&path_entry.path, &current),
                );
            }
        }
    }
    builder.finish()
}

impl DirEnvironment {
    /// Register a trusted root. Captures no environment state; the restore
    /// snapshot is taken later, at activation time.
    pub fn new(path: String) -> Self {
        DirEnvironment { path, active: None }
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    #[cfg(test)]
    pub(crate) fn restore_len(&self) -> usize {
        self.active.as_ref().map_or(0, |a| a.restore.len())
    }

    /// Load this root's entries without touching any runtime state.
    fn read_env_file(&self) -> Result<Vec<Entry>> {
        let root = PathBuf::from(&self.path);
        let env_file = root.join(".env");
        let envrc_file = root.join(".envrc");
        if env_file.exists() {
            if let Some(file) = env_file.to_str() {
                return read_env_config_file(file);
            }
        } else if envrc_file.exists()
            && let Some(file) = envrc_file.to_str()
        {
            return read_envrc_config_file(file);
        }
        Ok(Vec::new())
    }

    /// Parse, build the complete patch, commit it, then mark active.
    /// `None` when already active: the original snapshot is never retaken.
    /// Overlay values are exported: `.env`/`.envrc` entries are an
    /// environment overlay, visible to children while active.
    fn activate(&mut self, env: &mut Environment) -> Result<Option<DirenvEvent>> {
        if self.active.is_some() {
            return Ok(None);
        }
        let entries = self.read_env_file()?;
        let patch = build_patch(&entries, env);
        for (key, value) in &patch.values {
            env.set_and_export_shell_var(key.clone(), value.clone());
        }
        let exported = patch.values.iter().map(|(key, _)| key.clone()).collect();
        self.active = Some(ActiveDirEnvironment {
            restore: patch.restore,
        });
        Ok(Some(DirenvEvent::Loaded {
            path: self.path.clone(),
            exported,
        }))
    }

    /// Restore the exact activation-time logical state, then mark inactive.
    /// `None` when already inactive.
    fn deactivate(&mut self, env: &mut Environment) -> Option<DirenvEvent> {
        let active = self.active.take()?;
        // Reverse touch order: unwind the overlay in reverse apply order.
        for saved in active.restore.iter().rev() {
            env.restore_shell_var_state(&saved.key, &saved.previous);
        }
        Some(DirenvEvent::Unloaded {
            path: self.path.clone(),
        })
    }
}

fn prepend_path_entry(dir: &str, current: &str) -> String {
    if current.is_empty() {
        dir.to_string()
    } else {
        format!("{dir}:{current}")
    }
}

fn read_env_config_file(file: &str) -> Result<Vec<Entry>> {
    let mut ret: Vec<Entry> = Vec::new();
    let contents = fs::read_to_string(file)?;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let key = raw_key.trim().to_uppercase().to_string();
        let value = raw_value.trim().to_string();
        ret.push(Entry::Env(EnvEntry { key, value }));
    }
    Ok(ret)
}

fn read_envrc_config_file(file: &str) -> Result<Vec<Entry>> {
    let mut ret: Vec<Entry> = Vec::new();
    let contents = fs::read_to_string(file)?;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() < 2 {
            continue;
        }
        let cmd = parts[0].trim().to_uppercase().to_string();
        let value = parts[1].trim().to_string();

        match cmd.as_str() {
            "PATH_ADD" => ret.push(Entry::PathAdd(PathAddEntry { path: value })),
            "EXPORT" => {
                let Some((raw_key, raw_value)) = value.split_once('=') else {
                    continue;
                };
                let key = raw_key.trim().to_uppercase().to_string();
                let mut value = raw_value.trim().to_string();

                // Strip quotes if present
                if ((value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\'')))
                    && value.len() >= 2
                {
                    value = value[1..value.len() - 1].to_string();
                }

                ret.push(Entry::Env(EnvEntry { key, value }));
            }
            _ => {}
        }
    }
    Ok(ret)
}

fn root_depth(path: &str) -> usize {
    Path::new(path).components().count()
}

fn reconcile_roots(
    pwd: &Path,
    roots: &mut [DirEnvironment],
    env: &mut Environment,
) -> Result<Vec<DirenvEvent>> {
    // Unload phase first: deepest first, registration index LIFO on ties,
    // so nested overlays unwind in exact reverse apply order. Loading only
    // afterwards guarantees a new root snapshots the restored base, never
    // the previous root's overlay.
    let mut unload: Vec<(usize, usize)> = roots
        .iter()
        .enumerate()
        .filter(|(_, root)| root.is_active() && !pwd.starts_with(&root.path))
        .map(|(index, root)| (root_depth(&root.path), index))
        .collect();
    unload.sort_by(|a, b| b.cmp(a));

    let mut events = Vec::new();
    for (_, index) in unload {
        if let Some(event) = roots[index].deactivate(env) {
            events.push(event);
        }
    }

    // Load phase: shallowest first, registration order on ties, so an
    // inner root always snapshots its outer root's overlay.
    let mut load: Vec<(usize, usize)> = roots
        .iter()
        .enumerate()
        .filter(|(_, root)| !root.is_active() && pwd.starts_with(&root.path))
        .map(|(index, root)| (root_depth(&root.path), index))
        .collect();
    load.sort();

    for (_, index) in load {
        if let Some(event) = roots[index].activate(env)? {
            events.push(event);
        }
    }
    Ok(events)
}

/// Reconcile direnv overlays against `pwd`, returning presentation events.
///
/// Moves `direnv_roots` out of the environment for the reconciliation and
/// always moves it back, error or not, so a failed activation can never
/// lose an allowed root.
///
/// This take is sound because nothing called during reconciliation
/// (`set_and_export_shell_var` / `restore_shell_var_state` and the
/// derived-state refreshes behind them) reads `direnv_roots`. If a future
/// derived-state hook starts consulting the allow-list, this ownership
/// split must be revisited.
fn reconcile_path(pwd: &Path, environment: &mut Environment) -> Result<Vec<DirenvEvent>> {
    let mut roots = std::mem::take(&mut environment.variable_state.direnv_roots);
    let result = reconcile_roots(pwd, &mut roots, environment);
    environment.variable_state.direnv_roots = roots;
    result
}

pub fn check_path(pwd: &Path, environment: Arc<RwLock<Environment>>) -> Result<()> {
    let events = reconcile_path(pwd, &mut environment.write());

    let out = std::io::stdout().lock();
    let mut out = BufWriter::new(out);
    for event in &events? {
        match event {
            DirenvEvent::Loaded { path, exported } => {
                out.write_fmt(format_args!("direnv: loading {path}\n")).ok();
                out.write_all(b"direnv: export ").ok();
                for key in exported {
                    out.write_fmt(format_args!("+{key} ")).ok();
                }
                out.write_all(b"\n").ok();
            }
            DirenvEvent::Unloaded { path } => {
                out.write_fmt(format_args!("direnv: unloading {path}\n"))
                    .ok();
            }
        }
    }
    out.flush().ok();
    environment.write().reload_path();
    Ok(())
}

#[cfg(test)]
mod tests;
