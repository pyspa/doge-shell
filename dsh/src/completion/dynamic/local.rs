//! Table-driven collectors for local, fixed-shape dynamic completion providers.
//!
//! A provider belongs here when everything it needs is a compile-time
//! constant: which executable to run (or which path to read), with which
//! arguments, how to parse the output, how to describe a candidate, and
//! which directory to key the cache under. `platform.rs` is the same idea for
//! remote/API-backed CLIs (`CommandQueryPolicy::REMOTE`); this module is the
//! local (`CommandQueryPolicy::LOCAL`) half, and the two are the only places a
//! provider of either shape has to be written down.
//!
//! Rows live in the module that already owns the parser or loader function
//! they name and the path literals they contain (`CORE_LOCAL_SPECS` in
//! `dynamic.rs`, `LOCAL_SPECS` in `container.rs`/`dev.rs`/`linux.rs`/
//! `project.rs`). That keeps those functions private, and it keeps
//! `scripts/portability-allowlist.txt` unchanged: the allowlist is keyed by
//! file, so a Linux-only path literal that moves file is a CI failure even
//! though nothing about the behaviour changed. `spec_for` searches every
//! table, so which table a row sits in has no effect on dispatch - it is an
//! organisational split only, not a routing axis.
//!
//! `registry::ProviderRegistration::collect` consults this module before the
//! family collector, so a provider with a row here needs no `match` arm in a
//! family module and no prefix in `registry::family_for`.

use super::registry::DynamicProviderRequest;
use super::{DynamicCompletionProvider, canonicalize_path, run_command_lines, run_command_stdout};
use crate::completion::integrated::EnhancedCandidate;
use crate::completion::parser::ParsedCommandLine;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Which directory the cache entry is keyed under, and - for the command
/// sources - which directory the command runs in.
///
/// The distinction between `CurrentDir` and `CurrentDirCanonical` is
/// load-bearing: the collectors this table replaces are split roughly evenly
/// between `current_dir.to_path_buf()` and `canonicalize_path(&current_dir)`,
/// and the two produce different cache keys under a symlinked cwd.
#[derive(Debug, Clone, Copy)]
pub(super) enum Scope {
    /// A fixed absolute path used only as the cache key. The command runs in
    /// the caller's current directory.
    Fixed(&'static str),
    /// A fixed absolute path used as the cache key *and* as the command's
    /// working directory, for machine-global tools whose output does not
    /// depend on cwd.
    FixedCwd(&'static str),
    /// The caller's current directory, verbatim.
    CurrentDir,
    /// The caller's current directory, canonicalized.
    CurrentDirCanonical,
}

/// How the values are produced. Every variant keeps the existing
/// "missing command or unreadable path yields an empty list" behaviour: the
/// loader returns `Ok(Vec::new())` rather than an error.
#[derive(Debug, Clone, Copy)]
pub(super) enum Source {
    /// Run `executable args...`; feed the trimmed, non-empty stdout lines
    /// through `parser`.
    Lines {
        executable: &'static str,
        args: &'static [&'static str],
        parser: fn(&[String]) -> Vec<String>,
    },
    /// Run `executable args...`; feed raw stdout through `parser`.
    Stdout {
        executable: &'static str,
        args: &'static [&'static str],
        parser: fn(&str) -> Vec<String>,
    },
    /// Read a fixed path that is not the scope path. No subprocess.
    Path {
        path: &'static str,
        loader: fn(&Path) -> Vec<String>,
    },
    /// Read the scope path itself. No subprocess.
    ScopePath { loader: fn(&Path) -> Vec<String> },
    /// A loader that already knows its own sources. No subprocess, no path.
    Fixed { loader: fn() -> Vec<String> },
}

pub(super) struct LocalSpec {
    /// The `DynamicProviderId` string this row answers.
    pub(super) provider: &'static str,
    /// First half of the cache key. This is the *tool* name, which is not
    /// always the executable: `zypper.installed_package` reads through `rpm`,
    /// and the `lvm.*` providers read through `pvs`/`vgs`/`lvs`.
    pub(super) command_name: &'static str,
    /// Second half of the cache key.
    pub(super) value_kind: &'static str,
    pub(super) scope: Scope,
    pub(super) source: Source,
    pub(super) description: &'static str,
}

const SPEC_TABLES: &[&[LocalSpec]] = &[
    super::CORE_LOCAL_SPECS,
    super::container::LOCAL_SPECS,
    super::dev::LOCAL_SPECS,
    super::linux::LOCAL_SPECS,
    super::project::LOCAL_SPECS,
];

fn spec_for(provider: &str) -> Option<&'static LocalSpec> {
    SPEC_TABLES
        .iter()
        .copied()
        .flatten()
        .find(|spec| spec.provider == provider)
}

pub(super) fn collect(
    collector: &DynamicCompletionProvider,
    request: &DynamicProviderRequest<'_>,
) -> Option<Vec<EnhancedCandidate>> {
    let spec = spec_for(request.provider.as_str())?;
    Some(run(
        collector,
        spec,
        request.parsed_command_line,
        request.current_dir,
        request.cache_policy.is_cached_only(),
    ))
}

/// Every provider named by a `collect_by_id` call site, so the test below can
/// check the literals the compiler cannot. Keep in sync with the callers in
/// `dynamic.rs` (`collect_mount_candidates`, `collect_tmux_candidates`,
/// `collect_screen_candidates`, `collect_rustup_candidates`).
#[cfg(test)]
const COMMAND_LEVEL_CALL_SITES: &[&str] = &[
    "block.device",
    "fstab.mountpoint",
    "tmux.session",
    "screen.session",
    "rustup.toolchain",
];

/// For the handful of providers that also have a command-level entry point
/// (`DYNAMIC_PROVIDER_SPECS` in `integrated.rs`) alongside their declared
/// `DynamicProviderId`, so both routes answer from the same row instead of
/// one of them keeping a now-deleted collector method alive.
pub(super) fn collect_by_id(
    collector: &DynamicCompletionProvider,
    provider: &str,
    parsed_command_line: &ParsedCommandLine,
    current_dir: &Path,
    cached_only: bool,
) -> Vec<EnhancedCandidate> {
    match spec_for(provider) {
        Some(spec) => run(
            collector,
            spec,
            parsed_command_line,
            current_dir,
            cached_only,
        ),
        None => Vec::new(),
    }
}

fn run(
    collector: &DynamicCompletionProvider,
    spec: &LocalSpec,
    parsed_command_line: &ParsedCommandLine,
    current_dir: &Path,
    cached_only: bool,
) -> Vec<EnhancedCandidate> {
    let scope_dir = match spec.scope {
        Scope::Fixed(path) | Scope::FixedCwd(path) => PathBuf::from(path),
        Scope::CurrentDir => current_dir.to_path_buf(),
        Scope::CurrentDirCanonical => canonicalize_path(current_dir),
    };
    let workdir = match spec.scope {
        Scope::FixedCwd(path) => PathBuf::from(path),
        _ => current_dir.to_path_buf(),
    };
    let source = spec.source;
    // Resolved before the closure, exactly as every collector this replaces did.
    let command_path = match source {
        Source::Lines { executable, .. } | Source::Stdout { executable, .. } => {
            collector.resolve_command_path(executable)
        }
        _ => None,
    };
    let scope_for_loader = scope_dir.clone();

    collector.collect_cached_value_candidates(
        spec.command_name,
        spec.value_kind,
        scope_dir,
        parsed_command_line.current_token.as_str(),
        spec.description,
        cached_only,
        move || -> Result<Vec<String>> {
            match source {
                Source::Lines { args, parser, .. } => {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    Ok(parser(&run_command_lines(&command_path, args, &workdir)?))
                }
                Source::Stdout { args, parser, .. } => {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };
                    Ok(parser(&run_command_stdout(&command_path, args, &workdir)?))
                }
                Source::Path { path, loader } => Ok(loader(Path::new(path))),
                Source::ScopePath { loader } => Ok(loader(&scope_for_loader)),
                Source::Fixed { loader } => Ok(loader()),
            }
        },
    )
}

/// Preserves the collectors that returned `run_command_lines`'s output
/// untouched, as opposed to `parse_non_empty_lines`, which also sorts and
/// deduplicates.
pub(super) fn identity_lines(lines: &[String]) -> Vec<String> {
    lines.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::completion::DynamicProviderId;
    use std::collections::HashSet;

    /// A misspelled `provider` in a row is otherwise invisible: once a
    /// provider's family `match` arm is gone, a typo here just means the
    /// provider silently answers nothing instead of falling through to a
    /// missing arm (which would fail to compile).
    #[test]
    fn every_local_spec_names_a_registered_provider() {
        for spec in SPEC_TABLES.iter().copied().flatten() {
            assert!(
                DynamicProviderId::parse(spec.provider).is_some(),
                "local spec '{}' is not in DYNAMIC_COMPLETION_PROVIDERS",
                spec.provider
            );
        }
    }

    /// Rows are split across family modules for parser/loader visibility,
    /// not for routing, so nothing stops two modules claiming the same id.
    /// Whichever table `spec_for` reaches first would silently win.
    #[test]
    fn local_spec_providers_are_unique() {
        let mut seen = HashSet::new();
        for spec in SPEC_TABLES.iter().copied().flatten() {
            assert!(
                seen.insert(spec.provider),
                "duplicate local spec '{}'",
                spec.provider
            );
        }
    }

    /// `collect_by_id` takes a bare `&str`, so the command-level entry points
    /// that name a row (`collect_mount_candidates`, `collect_tmux_candidates`,
    /// ...) get no compiler check. A row renamed or moved out from under one of
    /// them would compile clean and silently answer nothing forever.
    #[test]
    fn the_command_level_call_sites_name_rows_that_exist() {
        for provider in COMMAND_LEVEL_CALL_SITES {
            assert!(
                spec_for(provider).is_some(),
                "'{provider}' is passed to collect_by_id but has no LocalSpec row"
            );
        }
    }

    /// A provider must use exactly one route. `ProviderRegistration::collect`
    /// consults the tables first, so an arm added later for a provider that
    /// already has a row becomes dead code with no warning and no failing
    /// build - the table just keeps winning.
    #[test]
    fn a_table_driven_provider_never_also_has_a_family_arm() {
        use crate::completion::dynamic::CachePolicy;
        use crate::completion::dynamic::registry;
        use crate::completion::parser::CommandLineParser;
        use crate::environment::Environment;

        let collector = DynamicCompletionProvider::new(Environment::new());
        let parsed = CommandLineParser::new().parse("", 0);
        let current_dir = std::env::current_dir().unwrap();

        for spec in SPEC_TABLES.iter().copied().flatten() {
            let registration = registry::registration(spec.provider)
                .unwrap_or_else(|| panic!("'{}' is not registered", spec.provider));
            let request = DynamicProviderRequest {
                provider: registration.id,
                scope: None,
                parsed_command_line: &parsed,
                current_dir: current_dir.as_path(),
                // Never spawns: the family arm we are looking for would have to
                // answer from cache, and an absent arm falls through to
                // `platform::collect`, which returns `None` for a local id.
                cache_policy: CachePolicy::CachedOnly,
            };
            assert!(
                (registration.collector)(&collector, &request).is_none(),
                "'{}' has both a LocalSpec row and a {:?} family match arm; the row \
                 silently wins, so the arm is dead code",
                spec.provider,
                registration.family
            );
        }
    }
}
