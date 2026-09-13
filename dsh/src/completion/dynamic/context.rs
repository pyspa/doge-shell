//! Questions about the command line the collectors need answered before they
//! can run: which words make up the invocation, which archive a `tar`/`unzip`
//! call is reading, which systemd unit kind and manager scope a `systemctl`
//! subcommand implies, and the two env-var truthiness readers.
use super::*;

pub(super) fn completion_words(parsed_command_line: &ParsedCommandLine) -> Vec<&str> {
    parsed_command_line
        .subcommand_path
        .iter()
        .chain(parsed_command_line.raw_args.iter())
        .map(String::as_str)
        .collect()
}

pub(super) fn tar_reads_archive(parsed_command_line: &ParsedCommandLine) -> bool {
    completion_words(parsed_command_line)
        .into_iter()
        .any(|word| {
            matches!(
                word,
                "-x" | "--extract" | "--get" | "-t" | "--list" | "x" | "t"
            ) || word
                .strip_prefix('-')
                .filter(|flags| !flags.starts_with('-'))
                .is_some_and(|flags| flags.contains('x') || flags.contains('t'))
        })
}

pub(super) fn selected_tar_archive(
    parsed_command_line: &ParsedCommandLine,
    current_dir: &Path,
) -> Option<PathBuf> {
    let words = completion_words(parsed_command_line);
    for (index, word) in words.iter().enumerate() {
        if let Some(value) = word.strip_prefix("--file=") {
            return archive_path_from_token(current_dir, value);
        }
        if matches!(*word, "-f" | "--file") {
            return words
                .get(index + 1)
                .and_then(|value| archive_path_from_token(current_dir, value));
        }
        if word
            .strip_prefix('-')
            .filter(|flags| !flags.starts_with('-'))
            .is_some_and(|flags| flags.contains('f'))
            || (!word.starts_with('-')
                && word.chars().all(|ch| ch.is_ascii_alphabetic())
                && word.contains('f')
                && (word.contains('x') || word.contains('t')))
        {
            return words
                .get(index + 1)
                .and_then(|value| archive_path_from_token(current_dir, value));
        }
    }
    None
}

pub(super) fn selected_unzip_archive(
    parsed_command_line: &ParsedCommandLine,
    current_dir: &Path,
) -> Option<PathBuf> {
    parsed_command_line
        .specified_arguments
        .first()
        .filter(|value| !value.is_empty() && *value != &parsed_command_line.current_token)
        .and_then(|value| archive_path_from_token(current_dir, value))
}

pub(super) fn archive_path_from_token(current_dir: &Path, token: &str) -> Option<PathBuf> {
    if token.is_empty() || token.starts_with('-') {
        return None;
    }
    let path = PathBuf::from(normalize_path_token(token));
    Some(if path.is_absolute() {
        path
    } else {
        current_dir.join(path)
    })
}

pub(super) fn archive_file_candidates(current_token: &str) -> Vec<EnhancedCandidate> {
    FileSystemGenerator::generate_file_candidates(current_token)
        .unwrap_or_default()
        .into_iter()
        .map(|candidate| {
            let candidate_type = match candidate.completion_type {
                CompletionType::Directory => CandidateType::Directory,
                _ => CandidateType::File,
            };
            EnhancedCandidate {
                text: candidate.text,
                description: candidate.description,
                candidate_type,
                priority: candidate.priority,
            }
        })
        .collect()
}

pub(super) fn systemctl_unit_kind_for_context(
    parsed_command_line: &ParsedCommandLine,
) -> SystemdUnitListKind {
    parsed_command_line
        .subcommand_path
        .first()
        .and_then(|subcommand| systemctl_unit_kind_for_subcommand(subcommand))
        .unwrap_or(SystemdUnitListKind::All)
}

pub(super) fn systemctl_unit_kind_for_subcommand(subcommand: &str) -> Option<SystemdUnitListKind> {
    match subcommand {
        "start" => Some(SystemdUnitListKind::UnitFiles),
        "stop" | "restart" | "reload" => Some(SystemdUnitListKind::Running),
        "enable" => Some(SystemdUnitListKind::Disabled),
        "disable" => Some(SystemdUnitListKind::Enabled),
        "status" | "is-active" | "is-enabled" | "mask" | "unmask" => Some(SystemdUnitListKind::All),
        _ => None,
    }
}

/// Maps a `systemctl.unit` / `systemctl.unit_file` provider scope onto the
/// matching `systemctl --type=` filter, so a JSON definition can narrow the
/// candidates to timers, sockets, slices and so on.
pub(super) fn systemd_unit_type_filter(scope: Option<&str>) -> Option<&'static str> {
    match scope? {
        "service" => Some("--type=service"),
        "socket" => Some("--type=socket"),
        "timer" => Some("--type=timer"),
        "slice" => Some("--type=slice"),
        "target" => Some("--type=target"),
        "mount" => Some("--type=mount"),
        "automount" => Some("--type=automount"),
        "path" => Some("--type=path"),
        "swap" => Some("--type=swap"),
        "scope" => Some("--type=scope"),
        "device" => Some("--type=device"),
        _ => None,
    }
}

pub(super) fn selected_systemd_manager_scope(
    parsed_command_line: &ParsedCommandLine,
) -> Option<SystemdManagerScope> {
    if matches!(
        &parsed_command_line.completion_context,
        CompletionContext::OptionValue { option_name, .. } if option_name == "--user-unit"
    ) {
        return Some(SystemdManagerScope::User);
    }

    let has_option = |name: &str| {
        parsed_command_line
            .specified_options
            .iter()
            .chain(parsed_command_line.raw_args.iter())
            .any(|token| token == name)
    };
    let has_inline_option_value = |name: &str| {
        parsed_command_line.raw_args.iter().any(|token| {
            token
                .strip_prefix(name)
                .is_some_and(|suffix| suffix.starts_with('='))
        })
    };

    if has_option("--user-unit") || has_inline_option_value("--user-unit") || has_option("--user") {
        Some(SystemdManagerScope::User)
    } else if has_option("--global") {
        Some(SystemdManagerScope::Global)
    } else if has_option("--system") {
        Some(SystemdManagerScope::System)
    } else {
        None
    }
}

pub(super) fn input_prefix_at_cursor(input: &str, cursor_pos: usize) -> String {
    input.chars().take(cursor_pos).collect()
}

pub(super) fn env_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub(super) fn env_falsey(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}
