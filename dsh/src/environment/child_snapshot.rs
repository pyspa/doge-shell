//! `ChildShellSnapshot`: the parent state a re-exec helper inherits.
//!
//! A `fork()` child cannot safely read the parent's `Environment` (locks do
//! not survive), and serializing the whole `Environment` would drag live
//! objects across — `RwLock`s, the MCP manager with its connections, the chat
//! client, the lifecycle worker, the history writer thread, the secret
//! manager runtime, completion caches. This snapshot is the opposite: plain
//! data only, captured under a read lock in the parent and applied to a fresh
//! `Environment::new()` in the helper. The snapshot is authoritative — the
//! helper never re-runs `config.lisp`, so aliases and variables cannot drift
//! or double-apply side effects.
//!
//! Deliberately independent from the Lisp rollback `EnvironmentSnapshot`
//! (`dsh/src/lisp/mod.rs`): that one rolls back configuration of a live
//! session, this one boots an isolated helper.

use crate::environment::Environment;
use crate::safety::SafetyLevel;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Plain-data shell state for one re-exec helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChildShellSnapshot {
    pub cwd: PathBuf,
    pub last_exit_status: i32,
    /// `$!` string value at capture time: the helper inherits what `$!`
    /// expands to, but never the parent's wait ownership (see
    /// `KnownAsyncLedger`: it is per-`Shell` and never snapshotted).
    pub last_async_pid: Option<i32>,
    pub variables: HashMap<String, String>,
    pub exported_vars: HashSet<String>,
    pub system_env_vars: HashMap<String, String>,
    pub aliases: HashMap<String, String>,
    pub abbreviations: HashMap<String, String>,
    pub command_abbreviations: HashMap<String, HashMap<String, String>>,
    pub paths: Vec<String>,
    pub z_exclude: Vec<String>,
    pub dir_stack: Vec<String>,
    /// Serialized as `"strict"`/`"normal"`/`"loose"`; unknown reads back as
    /// `Normal` (fail closed, never `Loose`).
    pub safety_level: String,
    pub execute_allowlist: Vec<String>,
    pub shell_always_allowlist: Vec<String>,
}

fn safety_to_str(level: SafetyLevel) -> String {
    match level {
        SafetyLevel::Strict => "strict",
        SafetyLevel::Normal => "normal",
        SafetyLevel::Loose => "loose",
    }
    .to_string()
}

fn safety_from_str(raw: &str) -> SafetyLevel {
    match raw {
        "strict" => SafetyLevel::Strict,
        "loose" => SafetyLevel::Loose,
        _ => SafetyLevel::Normal,
    }
}

impl ChildShellSnapshot {
    /// Capture parent state under a read lock. No live objects cross.
    pub fn capture(env: &Environment) -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            last_exit_status: env.last_exit_status,
            last_async_pid: env.last_async_pid,
            variables: env.variable_state.variables.clone(),
            exported_vars: env.variable_state.exported_vars.clone(),
            system_env_vars: env.variable_state.system_env_vars.clone(),
            aliases: env.variable_state.alias.clone(),
            abbreviations: env.variable_state.abbreviations.clone(),
            command_abbreviations: env.variable_state.command_abbreviations.clone(),
            paths: env.variable_state.paths.clone(),
            z_exclude: env.variable_state.z_exclude.clone(),
            dir_stack: env.dir_stack.clone(),
            safety_level: safety_to_str(*env.policy_state.safety_level.read()),
            execute_allowlist: env.policy_state.execute_allowlist.read().clone(),
            shell_always_allowlist: env.policy_state.shell_always_allowlist.read().clone(),
        }
    }

    /// Apply onto a fresh environment, then refresh derived state (`PATH`,
    /// `Z_EXCLUDE`, AI config slots follow the same refresh path as the
    /// normal startup). Afterwards `chdir` to the snapshot cwd — the helper
    /// must not depend on `posix_spawn` cwd inheritance alone.
    pub fn apply_to(&self, env: &mut Environment) {
        env.last_exit_status = self.last_exit_status;
        env.last_async_pid = self.last_async_pid;
        env.variable_state.variables = self.variables.clone();
        env.variable_state.exported_vars = self.exported_vars.clone();
        env.variable_state.system_env_vars = self.system_env_vars.clone();
        env.variable_state.alias = self.aliases.clone();
        env.variable_state.abbreviations = self.abbreviations.clone();
        env.variable_state.command_abbreviations = self.command_abbreviations.clone();
        env.variable_state.paths = self.paths.clone();
        env.variable_state.z_exclude = self.z_exclude.clone();
        env.dir_stack = self.dir_stack.clone();
        *env.policy_state.safety_level.write() = safety_from_str(&self.safety_level);
        *env.policy_state.execute_allowlist.write() = self.execute_allowlist.clone();
        *env.policy_state.shell_always_allowlist.write() = self.shell_always_allowlist.clone();
        env.refresh_derived_state("PATH");
        env.refresh_derived_state("Z_EXCLUDE");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips_shell_state() {
        let env_arc = Environment::new();
        {
            let mut env = env_arc.write();
            env.set_shell_var("SNAP_VAR".to_string(), "snap_value".to_string());
            env.variable_state
                .exported_vars
                .insert("SNAP_VAR".to_string());
            env.variable_state
                .alias
                .insert("ll".to_string(), "ls -l".to_string());
            env.last_exit_status = 3;
            env.last_async_pid = Some(424242);
            *env.policy_state.safety_level.write() = SafetyLevel::Strict;
        }
        let snapshot = ChildShellSnapshot::capture(&env_arc.read());
        let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let back: ChildShellSnapshot = serde_json::from_str(&json).expect("deserialize snapshot");
        assert_eq!(
            back.variables.get("SNAP_VAR"),
            Some(&"snap_value".to_string())
        );
        assert!(back.exported_vars.contains("SNAP_VAR"));
        assert_eq!(back.aliases.get("ll"), Some(&"ls -l".to_string()));
        assert_eq!(back.last_exit_status, 3);
        assert_eq!(back.last_async_pid, Some(424242));
        assert_eq!(back.safety_level, "strict");

        let fresh = Environment::new();
        {
            let mut env = fresh.write();
            back.apply_to(&mut env);
            assert_eq!(
                env.variable_state.variables.get("SNAP_VAR"),
                Some(&"snap_value".to_string())
            );
            assert_eq!(env.last_exit_status, 3);
            assert_eq!(env.last_async_pid, Some(424242));
            assert_eq!(*env.policy_state.safety_level.read(), SafetyLevel::Strict);
        }
    }

    #[test]
    fn unknown_safety_level_fails_closed_to_normal() {
        assert_eq!(safety_from_str("anything-else"), SafetyLevel::Normal);
    }
}
