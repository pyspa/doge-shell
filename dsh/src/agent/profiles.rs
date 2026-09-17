//! Named `--allow-command` bundles for `agent run`/`resume`/`retry`.
//!
//! A profile expands to exact command lines before the task is persisted,
//! so the stored grant stays auditable in `agent show` and the detached
//! child needs no new execution path. Profiles never bypass hard denials
//! such as skill-script approvals.

use anyhow::Result;
use dsh_types::agent::TaskGrant;

/// Exact commands granted by the `rust-build` profile.
pub(crate) const RUST_BUILD_COMMANDS: &[&str] = &[
    "cargo build",
    "cargo test",
    "cargo check",
    "cargo check --workspace",
];

/// Available profiles with a one-line description each.
pub(crate) fn list() -> Vec<(&'static str, &'static str)> {
    vec![(
        "rust-build",
        "cargo build/test/check for this project (exact match; use --timeout 1800 for large builds)",
    )]
}

/// Exact commands for a profile name.
pub(crate) fn expand(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "rust-build" => Some(RUST_BUILD_COMMANDS),
        _ => None,
    }
}

/// Expands profile names into `grant.commands`, skipping duplicates.
/// Returns the number of commands added.
pub(crate) fn apply(grant: &mut TaskGrant, names: &[String]) -> Result<usize> {
    let mut added = 0;
    for name in names {
        let commands = expand(name)
            .ok_or_else(|| anyhow::anyhow!("unknown agent profile `{name}` (see `agent profiles`)"))?;
        for command in commands {
            if !grant.commands.iter().any(|c| c == command) {
                grant.commands.push(command.to_string());
                added += 1;
            }
        }
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_profile_is_an_error() {
        let mut grant = TaskGrant::default();
        assert!(apply(&mut grant, &["nope".to_string()]).is_err());
        assert!(grant.commands.is_empty());
    }

    #[test]
    fn rust_build_expands_without_duplicates() {
        let mut grant = TaskGrant::default();
        grant.commands.push("cargo build".to_string());
        let added = apply(&mut grant, &["rust-build".to_string()]).unwrap();
        assert_eq!(added, 3);
        assert_eq!(
            grant.commands,
            vec![
                "cargo build".to_string(),
                "cargo test".to_string(),
                "cargo check".to_string(),
                "cargo check --workspace".to_string(),
            ]
        );
        // Applying twice adds nothing.
        assert_eq!(apply(&mut grant, &["rust-build".to_string()]).unwrap(), 0);
    }

    #[test]
    fn list_contains_rust_build() {
        assert!(list().iter().any(|(name, _)| *name == "rust-build"));
        assert!(expand("rust-build").is_some());
    }
}
