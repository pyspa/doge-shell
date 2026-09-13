//! Reading the user's own configuration for candidates: cargo metadata,
//! the SSH config and known-hosts files, the man page tree, and the local
//! user/group names, plus the candidate text each is rendered as.
use super::*;

pub(super) fn parse_cargo_metadata_values(
    output: &str,
    kind: CargoMetadataValueKind,
) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };

    let mut values = Vec::new();
    let Some(packages) = value
        .get("packages")
        .and_then(|packages| packages.as_array())
    else {
        return Vec::new();
    };

    for package in packages {
        match kind {
            CargoMetadataValueKind::Package => {
                if let Some(name) = package.get("name").and_then(|name| name.as_str()) {
                    values.push(name.to_string());
                }
            }
            CargoMetadataValueKind::Feature => {
                if let Some(features) = package
                    .get("features")
                    .and_then(|features| features.as_object())
                {
                    values.extend(features.keys().cloned());
                }
            }
            CargoMetadataValueKind::Bin
            | CargoMetadataValueKind::Example
            | CargoMetadataValueKind::Test
            | CargoMetadataValueKind::Bench => {
                let Some(targets) = package
                    .get("targets")
                    .and_then(|targets| targets.as_array())
                else {
                    continue;
                };
                let expected_kind = match kind {
                    CargoMetadataValueKind::Bin => "bin",
                    CargoMetadataValueKind::Example => "example",
                    CargoMetadataValueKind::Test => "test",
                    CargoMetadataValueKind::Bench => "bench",
                    CargoMetadataValueKind::Package | CargoMetadataValueKind::Feature => {
                        unreachable!()
                    }
                };
                for target in targets {
                    let Some(kinds) = target.get("kind").and_then(|kinds| kinds.as_array()) else {
                        continue;
                    };
                    let has_kind = kinds
                        .iter()
                        .any(|target_kind| target_kind.as_str() == Some(expected_kind));
                    if has_kind
                        && let Some(name) = target.get("name").and_then(|name| name.as_str())
                    {
                        values.push(name.to_string());
                    }
                }
            }
        }
    }

    dedup_sorted(values)
}

pub(super) fn cargo_feature_token_parts(token: &str) -> (&str, &str) {
    token
        .rfind(',')
        .map(|comma| token.split_at(comma + 1))
        .unwrap_or(("", token))
}

pub(super) fn ssh_config_scope() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".ssh"))
        .unwrap_or_else(|| PathBuf::from(".ssh"))
}

pub(super) fn load_ssh_hosts() -> Vec<String> {
    let mut values = Vec::new();
    if let Some(home) = dirs::home_dir() {
        values.extend(parse_ssh_config_hosts(
            &fs::read_to_string(home.join(".ssh").join("config")).unwrap_or_default(),
        ));
        values.extend(parse_known_hosts(
            &fs::read_to_string(home.join(".ssh").join("known_hosts")).unwrap_or_default(),
        ));
    }
    dedup_sorted(values)
}

pub(super) fn man_page_roots(configured_manpath: Option<&str>) -> Vec<PathBuf> {
    let mut roots = configured_manpath
        .filter(|value| !value.trim().is_empty())
        .map(|value| std::env::split_paths(value).collect::<Vec<_>>())
        .unwrap_or_default();
    roots.extend([
        PathBuf::from("/usr/local/share/man"),
        PathBuf::from("/usr/share/man"),
    ]);
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".local/share/man"));
    }
    roots.sort();
    roots.dedup();
    roots.retain(|path| path.is_dir());
    roots
}

pub(super) fn load_man_page_names(roots: &[PathBuf]) -> Vec<String> {
    let mut values = Vec::new();
    for root in roots {
        collect_man_page_names(root, 0, &mut values);
    }
    dedup_sorted(values)
}

pub(super) fn collect_man_page_names(dir: &Path, depth: usize, values: &mut Vec<String>) {
    if depth > 2 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_man_page_names(&path, depth + 1, values);
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(page) = man_page_name_from_file(file_name) {
            values.push(page.to_string());
        }
    }
}

pub(super) fn man_page_name_from_file(file_name: &str) -> Option<&str> {
    let mut stem = file_name;
    for extension in [".gz", ".xz", ".bz2", ".zst", ".lzma"] {
        if let Some(stripped) = stem.strip_suffix(extension) {
            stem = stripped;
            break;
        }
    }
    let (page, section) = stem.rsplit_once('.')?;
    (!page.is_empty() && !section.is_empty()).then_some(page)
}

/// `user` and `user:group` values for `chown` and `chgrp`.
///
/// Both sides come from the generators, which read `/etc/passwd` and
/// `/etc/group` on Linux and Open Directory on macOS; parsing the files here
/// too offered nothing but service accounts on macOS for owners, and missed
/// every directory-managed group for groups. Service accounts are wanted in
/// this list -- both `chown www-data` and `chown _www` are ordinary -- so
/// nothing is filtered out.
pub(super) fn load_owner_group_values() -> Vec<String> {
    let mut values = Vec::new();
    values.extend(
        crate::completion::generators::user::user_names(true)
            .into_iter()
            .map(|name| format!("u:{name}")),
    );
    values.extend(
        crate::completion::generators::group::group_names()
            .into_iter()
            .map(|name| format!("g:{name}")),
    );
    dedup_sorted(values)
}

pub(super) fn owner_group_candidates(
    values: &[String],
    current_token: &str,
) -> Vec<EnhancedCandidate> {
    let group_context = current_token.rsplit_once(':');
    values
        .iter()
        .filter_map(|encoded| {
            let (kind, value) = encoded.split_once(':')?;
            let (text, description) = if let Some((owner, group_prefix)) = group_context {
                if kind != "g" || !matches_prefix(group_prefix, value) {
                    return None;
                }
                (format!("{owner}:{value}"), "group")
            } else {
                if kind != "u" || !matches_prefix(current_token, value) {
                    return None;
                }
                (value.to_string(), "user")
            };
            Some(EnhancedCandidate {
                text,
                description: Some(description.to_string()),
                candidate_type: CandidateType::Argument,
                priority: 140,
            })
        })
        .collect()
}

pub(super) fn parse_ssh_config_hosts(contents: &str) -> Vec<String> {
    let mut values = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        if !parts
            .next()
            .is_some_and(|keyword| keyword.eq_ignore_ascii_case("host"))
        {
            continue;
        }
        for host in parts {
            if host.contains('*') || host.contains('?') || host.starts_with('!') {
                continue;
            }
            values.push(host.to_string());
        }
    }
    dedup_sorted(values)
}

pub(super) fn parse_known_hosts(contents: &str) -> Vec<String> {
    let mut values = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() || trimmed.starts_with('|') {
            continue;
        }
        let fields = trimmed.split_whitespace().collect::<Vec<_>>();
        let host_field = if fields.first().is_some_and(|field| field.starts_with('@')) {
            fields.get(1).copied()
        } else {
            fields.first().copied()
        };
        let Some(host_field) = host_field else {
            continue;
        };
        for host in host_field.split(',') {
            let host = if let Some(rest) = host.strip_prefix('[') {
                rest.split(']').next().unwrap_or(rest)
            } else {
                host.split(':').next().unwrap_or(host)
            };
            if !host.is_empty() && !host.starts_with('|') {
                values.push(host.to_string());
            }
        }
    }
    dedup_sorted(values)
}

pub(super) fn format_ssh_host_candidate_text(
    command_name: &str,
    user_prefix: Option<&str>,
    host: String,
) -> String {
    let mut text = if let Some(user) = user_prefix {
        format!("{user}@{host}")
    } else {
        host
    };
    if matches!(command_name, "scp" | "rsync") {
        text.push(':');
    }
    text
}
