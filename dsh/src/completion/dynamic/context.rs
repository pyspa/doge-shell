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

/// Whether this `modprobe` line unloads rather than loads.
///
/// `-r` is routinely bundled with its modifiers (`modprobe -rv`, `-nr`), and
/// matching the whole token -- as this used to -- left every bundled form
/// offering the whole `/lib/modules` list instead of the loaded modules `-r`
/// can actually remove.
///
/// Long options are excluded from the cluster scan on purpose: `--remove` is
/// modprobe's only long spelling of this, and `--dry-run` must not match on its
/// `r`.
pub(super) fn modprobe_removes_module(parsed_command_line: &ParsedCommandLine) -> bool {
    completion_words(parsed_command_line)
        .into_iter()
        // The token under the cursor is still being typed: `modprobe -r<TAB>`
        // is completing the flag itself, not a module name for it.
        .filter(|word| *word != parsed_command_line.current_token)
        .any(|word| {
            word == "--remove"
                || word
                    .strip_prefix('-')
                    .filter(|flags| !flags.starts_with('-'))
                    .is_some_and(|flags| flags.contains('r'))
        })
}

pub(super) fn selected_tar_archive(
    parsed_command_line: &ParsedCommandLine,
    current_dir: &Path,
) -> Option<PathBuf> {
    let raw_words = if let Some(source) = &parsed_command_line.source {
        source
            .argv()
            .map(|(index, word)| {
                if index == source.cursor_word {
                    ""
                } else {
                    word
                }
            })
            .collect()
    } else {
        completion_words(parsed_command_line)
    };
    let words: Vec<_> = raw_words
        .into_iter()
        .map(crate::completion::shell_path::decode_literal_word)
        .collect();
    let mut argv = words.iter().map(String::as_str).enumerate();
    let mut reads = false;
    let mut archive = None;
    while let Some((index, word)) = argv.next() {
        match word {
            "--" => break,
            "--create" | "--append" | "--update" => return None,
            "--extract" | "--get" | "--list" => reads = true,
            "--file" => archive = argv.next().map(|(_, value)| value),
            "--directory"
            | "--exclude"
            | "--exclude-from"
            | "--files-from"
            | "--blocking-factor"
            | "--use-compress-program" => {
                argv.next();
            }
            _ if word.starts_with("--file=") => archive = word.strip_prefix("--file="),
            _ => {
                // Traditional options (e.g. "xfz") take arguments from
                // subsequent words regardless of the position of 'f'.
                let traditional =
                    index == 0 && !word.is_empty() && word.bytes().all(|c| c.is_ascii_alphabetic());
                let flags = if traditional {
                    word
                } else if let Some(flags) = word.strip_prefix('-').filter(|s| !s.starts_with('-')) {
                    flags
                } else {
                    continue;
                };
                for (i, flag) in flags.char_indices() {
                    match flag {
                        'x' | 't' => reads = true,
                        'c' | 'r' | 'u' => return None,
                        'f' | 'C' | 'T' | 'X' | 'b' | 'I' => {
                            let tail = &flags[i + 1..];
                            let attached = !traditional && !tail.is_empty();
                            let value = if attached {
                                Some(tail)
                            } else {
                                argv.next().map(|(_, value)| value)
                            };
                            if flag == 'f' {
                                archive = value;
                            }
                            if attached {
                                break;
                            }
                        }
                        // No argument on the tar implementations that accept them.
                        'a' | 'B' | 'G' | 'h' | 'i' | 'j' | 'J' | 'k' | 'l' | 'm' | 'M' | 'n'
                        | 'o' | 'O' | 'p' | 'P' | 'q' | 'R' | 'S' | 'U' | 'v' | 'w' | 'y' | 'z'
                        | 'Z' => {}
                        // In particular H/L/s/W differ between GNU tar and bsdtar.
                        // Their suffix may be a value rather than more flags.
                        _ => break,
                    }
                }
            }
        }
    }
    archive
        .filter(|_| reads)
        .filter(|value| !value.is_empty() && *value != "-")
        .map(|value| current_dir.join(value))
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
                CompletionType::HomeFile => CandidateType::HomeFile,
                CompletionType::HomeDirectory => CandidateType::HomeDirectory,
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
