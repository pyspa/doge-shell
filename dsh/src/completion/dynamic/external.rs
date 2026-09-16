//! Completion for miscellaneous external CLI tools with no family of their
//! own: package managers (apt/pacman/brew), cloud CLIs (aws/az/gcloud),
//! ansible, btrfs, dmsetup, and the `DOGESH_EXTERNAL_COMPLETER` escape hatch.
use super::super::integrated::{CandidateType, EnhancedCandidate, matches_prefix};
use super::super::shell_path::normalize_path_token;
use super::{
    CachePolicy, DynamicCompletionProvider, ExternalCompletionCacheKey, ParsedCommandLine,
    canonicalize_path, dedup_sorted, parse_package_lines, run_command_lines,
    run_external_completer_for_key,
};
use std::path::{Path, PathBuf};
use tracing::warn;

pub(super) fn collect(
    collector: &super::DynamicCompletionProvider,
    request: &super::registry::DynamicProviderRequest<'_>,
) -> Option<Vec<EnhancedCandidate>> {
    use super::*;

    let provider = request.provider.as_str();
    let parsed_command_line = request.parsed_command_line;
    let current_dir = request.current_dir;
    let cached_only = request.cache_policy.is_cached_only();
    let current_token = parsed_command_line.current_token.as_str();

    Some(match provider {
        "brew.installed" => collector.collect_brew_installed_candidates(current_token, cached_only),
        "apt.installed_package" => collector.collect_apt_installed_package_candidates(
            current_dir,
            current_token,
            parsed_command_line.command.as_str(),
            cached_only,
        ),
        "pacman.package" => match pacman_sync_mode(parsed_command_line) {
            Some(sync) => collector.collect_pacman_package_candidates(
                current_dir,
                current_token,
                sync,
                cached_only,
            ),
            None => Vec::new(),
        },
        "ansible.inventory_host" => collector.collect_ansible_inventory_host_candidates(
            parsed_command_line,
            current_dir,
            current_token,
            cached_only,
        ),
        "aws.profile" => collector.collect_aws_profile_candidates(current_token, cached_only),
        "az.subscription" => {
            collector.collect_az_subscription_candidates(current_token, cached_only)
        }
        "gcloud.configuration" => {
            collector.collect_gcloud_configuration_candidates(current_token, cached_only)
        }
        "gcloud.project" => collector.collect_gcloud_project_candidates(current_token, cached_only),
        "btrfs.subvolume" => {
            collector.collect_btrfs_subvolume_candidates(current_dir, current_token, cached_only)
        }
        "dmsetup.device" => collector.collect_dmsetup_device_candidates(current_token, cached_only),
        _ => {
            return platform::collect(
                collector,
                provider,
                parsed_command_line,
                current_dir,
                cached_only,
            );
        }
    })
}

impl DynamicCompletionProvider {
    pub(crate) fn collect_external_candidates(
        &self,
        current_dir: &Path,
        input: &str,
        cursor_pos: usize,
        parsed_command_line: &ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        let Some(command_template) = self
            .environment
            .read()
            .get_var("DOGESH_EXTERNAL_COMPLETER")
            .filter(|value| !value.trim().is_empty())
        else {
            return Vec::new();
        };

        let subcommand_path = parsed_command_line.subcommand_path.join(" ");
        let key = ExternalCompletionCacheKey {
            command_template: command_template.clone(),
            current_dir: canonicalize_path(current_dir),
            input: input.to_string(),
            cursor_pos,
            command: parsed_command_line.command.clone(),
            current_token: parsed_command_line.current_token.clone(),
            subcommand_path,
        };

        let loader_key = key.clone();
        match self
            .load_external_candidates(key, move || run_external_completer_for_key(&loader_key))
        {
            Ok(candidates) => candidates,
            Err(err) => {
                warn!("External completer failed: {}", err);
                Vec::new()
            }
        }
    }

    pub(crate) fn collect_pacman_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let Some(sync) = pacman_sync_mode(parsed_command_line) else {
            return Vec::new();
        };
        self.collect_pacman_package_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            sync,
            cached_only,
        )
    }
    fn collect_pacman_package_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        sync: bool,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let (kind, args, description) = if sync {
            ("sync-package", vec!["-Slq"], "pacman sync package")
        } else {
            ("installed-package", vec!["-Qq"], "installed pacman package")
        };
        let command_path = self.resolve_command_path("pacman");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "pacman",
            kind,
            canonicalize_path(&current_dir),
            current_token,
            description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                run_command_lines(&command_path, &args, &current_dir)
            },
        )
    }
    fn collect_apt_installed_package_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        command_name: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("dpkg-query");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            command_name,
            "installed-package",
            PathBuf::from("/var/lib/dpkg/status"),
            current_token,
            "installed deb package",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_package_lines(&run_command_lines(
                    &command_path,
                    &["-W", "-f=${binary:Package}\\n"],
                    &current_dir,
                )?))
            },
        )
    }
    /// Homebrew-installed formulae and casks (`brew list`), for
    /// `brew uninstall`/`brew upgrade` completion. Global (no project scope).
    fn collect_brew_installed_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("brew");
        // brew is machine-global; use a fixed scope so the cache is shared
        // across working directories.
        let scope_dir = PathBuf::from("/");
        self.collect_cached_value_candidates(
            "brew",
            "installed",
            scope_dir,
            current_token,
            "brew installed",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let mut values =
                    run_command_lines(&command_path, &["list", "--formula"], Path::new("/"))?;
                if let Ok(casks) =
                    run_command_lines(&command_path, &["list", "--cask"], Path::new("/"))
                {
                    values.extend(casks);
                }
                Ok(dedup_sorted(values))
            },
        )
    }
}

pub(super) fn parse_line(line: &str, current_token: &str) -> Option<EnhancedCandidate> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('{')
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(object) = value.as_object()
    {
        let text = object.get("text").and_then(|value| value.as_str())?;
        let replacement = object
            .get("replacement")
            .and_then(|value| value.as_str())
            .unwrap_or(text)
            .trim();
        if replacement.is_empty() || !matches_prefix(current_token, replacement) {
            return None;
        }

        let mut description = object
            .get("description")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if description.is_none() && replacement != text {
            description = Some(text.to_string());
        }

        let candidate_type = object
            .get("type")
            .and_then(|value| value.as_str())
            .and_then(parse_candidate_type)
            .unwrap_or(CandidateType::Argument);
        let priority = object
            .get("priority")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(200);

        return Some(EnhancedCandidate {
            text: replacement.to_string(),
            description,
            candidate_type,
            priority,
        });
    }

    let (text, description) = if let Some((text, description)) = trimmed.split_once('\t') {
        (text.trim(), Some(description.trim().to_string()))
    } else {
        (trimmed, None)
    };
    if text.is_empty() || !matches_fish_prefix(current_token, text) {
        return None;
    }
    Some(EnhancedCandidate {
        text: text.to_string(),
        description,
        candidate_type: CandidateType::Argument,
        priority: 200,
    })
}

pub(super) fn parse_fish_line(line: &str, current_token: &str) -> Option<EnhancedCandidate> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (text, description) = if let Some((text, description)) = trimmed.split_once('\t') {
        (text.trim(), Some(description.trim().to_string()))
    } else {
        (trimmed, None)
    };
    if text.is_empty() || !matches_fish_prefix(current_token, text) {
        return None;
    }
    let candidate_type = if text.ends_with('/') {
        CandidateType::Directory
    } else if text.starts_with("--") {
        CandidateType::LongOption
    } else if text.starts_with('-') {
        CandidateType::ShortOption
    } else {
        CandidateType::Argument
    };
    Some(EnhancedCandidate {
        text: text.to_string(),
        description,
        candidate_type,
        priority: 35,
    })
}

fn parse_candidate_type(value: &str) -> Option<CandidateType> {
    match value {
        "subcommand" | "SubCommand" => Some(CandidateType::SubCommand),
        "short-option" | "short_option" | "ShortOption" => Some(CandidateType::ShortOption),
        "long-option" | "long_option" | "LongOption" => Some(CandidateType::LongOption),
        "argument" | "Argument" => Some(CandidateType::Argument),
        "file" | "File" => Some(CandidateType::File),
        "directory" | "Directory" => Some(CandidateType::Directory),
        "process" | "Process" => Some(CandidateType::Process),
        "generic" | "Generic" => Some(CandidateType::Generic),
        _ => None,
    }
}

pub(super) fn matches_fish_prefix(current_token: &str, text: &str) -> bool {
    if matches_prefix(current_token, text) || text.starts_with(current_token) {
        return true;
    }
    let quote_stripped = current_token.trim_start_matches(['\'', '"']);
    if quote_stripped != current_token
        && (matches_prefix(quote_stripped, text) || text.starts_with(quote_stripped))
    {
        return true;
    }
    let normalized_current_token = normalize_path_token(current_token);
    normalized_current_token != current_token
        && (matches_prefix(&normalized_current_token, text)
            || text.starts_with(&normalized_current_token))
}

/// The pacman operation the command line selects, as its single upper-case
/// letter.
///
/// pacman spells operations as short flags that are routinely bundled with
/// their modifiers (`-Rns`, `-Syu`, `-Qi`) or written out in long form
/// (`--remove`). Matching `-R`/`-S` literally, as this used to, left every
/// bundled form with no candidates at all.
fn pacman_operation(parsed_command_line: &ParsedCommandLine) -> Option<char> {
    const OPERATIONS: [char; 7] = ['S', 'R', 'Q', 'U', 'F', 'D', 'T'];

    parsed_command_line
        .subcommand_path
        .iter()
        .chain(parsed_command_line.raw_args.iter())
        // The token under the cursor is still being typed: `pacman -R<TAB>` is
        // completing the flag itself, not a package name for it.
        .filter(|token| token.as_str() != parsed_command_line.current_token)
        .find_map(|token| match token.as_str() {
            "--sync" => Some('S'),
            "--remove" => Some('R'),
            "--query" => Some('Q'),
            "--upgrade" => Some('U'),
            "--files" => Some('F'),
            "--database" => Some('D'),
            "--deptest" => Some('T'),
            value if value.starts_with("--") => None,
            value => value
                .strip_prefix('-')
                .and_then(|flags| flags.chars().next())
                .filter(|flag| OPERATIONS.contains(flag)),
        })
}
/// Whether pacman package candidates should come from the sync repositories
/// (`true`, for installs) or from the local database (`false`).
///
/// Only the local database lists AUR/foreign packages, so every operation that
/// acts on already-installed packages must land on `false`. `None` means the
/// operation takes no package name and should offer nothing.
pub(crate) fn pacman_sync_mode(parsed_command_line: &ParsedCommandLine) -> Option<bool> {
    match pacman_operation(parsed_command_line)? {
        'S' => Some(true),
        'R' | 'Q' | 'F' | 'D' | 'T' => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_line_parser_preserves_declared_priority_and_type() {
        let candidate = parse_line(
            r#"{"text":"alpha","description":"from tool","type":"subcommand","priority":321}"#,
            "al",
        )
        .unwrap();

        assert_eq!(candidate.text, "alpha");
        assert_eq!(candidate.description.as_deref(), Some("from tool"));
        assert_eq!(candidate.candidate_type, CandidateType::SubCommand);
        assert_eq!(candidate.priority, 321);
    }
}
