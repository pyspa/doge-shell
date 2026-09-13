//! Ansible inventory file selection (`-i`/`--inventory`, falling back to the
//! project default) and value parsing (INI and YAML-ish inventories).
use super::*;
use crate::completion::parser::ParsedCommandLine;

pub(super) fn selected_ansible_inventory_paths(
    parsed_command_line: &ParsedCommandLine,
    current_dir: &Path,
    project_root: &Path,
) -> Vec<PathBuf> {
    let words = parsed_command_line
        .subcommand_path
        .iter()
        .chain(parsed_command_line.raw_args.iter())
        .map(String::as_str)
        .collect::<Vec<_>>();
    let mut values = Vec::new();
    for (index, token) in words.iter().enumerate() {
        if *token == "-i" || *token == "--inventory" {
            let Some(value) = words.get(index + 1).copied() else {
                continue;
            };
            if !value.is_empty() && !value.starts_with('-') {
                values.push(path_from_token(current_dir, value));
            }
            continue;
        }
        if let Some(value) = token
            .strip_prefix("--inventory=")
            .or_else(|| token.strip_prefix("-i="))
            && !value.is_empty()
        {
            values.push(path_from_token(current_dir, value));
        }
    }

    if values.is_empty() {
        values.extend([
            project_root.join("inventory"),
            project_root.join("hosts"),
            project_root.join("ansible").join("inventory"),
            current_dir.join("inventory"),
            current_dir.join("hosts"),
        ]);
    }
    dedup_sorted_paths(values)
}

pub(super) fn path_from_token(current_dir: &Path, token: &str) -> PathBuf {
    let path = PathBuf::from(normalize_path_token(token));
    if path.is_absolute() {
        path
    } else {
        current_dir.join(path)
    }
}

pub(super) fn load_ansible_inventory_values(paths: &[PathBuf]) -> Vec<String> {
    let mut values = Vec::new();
    for path in paths {
        values.extend(load_ansible_inventory_path(path));
    }
    dedup_sorted(values)
}

pub(super) fn load_ansible_inventory_path(path: &Path) -> Vec<String> {
    if path.is_file() {
        return parse_ansible_inventory_file(path);
    }
    if !path.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for entry in entries.flatten() {
        let child = entry.path();
        if child.is_file() {
            values.extend(parse_ansible_inventory_file(&child));
        }
    }
    values
}

pub(super) fn parse_ansible_inventory_file(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_ansible_inventory_values(&contents)
}

pub(super) fn parse_ansible_inventory_values(contents: &str) -> Vec<String> {
    let mut values = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if let Some(section) = trimmed.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
            let name = section
                .split(':')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches(['"', '\'']);
            if is_ansible_inventory_name(name) {
                values.push(name.to_string());
            }
            continue;
        }
        if let Some(key) = trimmed.strip_suffix(':') {
            let key = key.trim().trim_matches(['"', '\'']);
            if is_ansible_inventory_name(key)
                && !matches!(key, "all" | "hosts" | "children" | "vars")
            {
                values.push(key.to_string());
            }
            continue;
        }
        let host = trimmed
            .split_whitespace()
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches(['"', '\'']);
        if is_ansible_inventory_name(host)
            && !host.contains('=')
            && !matches!(host, "all" | "hosts" | "children" | "vars")
        {
            values.push(host.to_string());
        }
    }
    dedup_sorted(values)
}

pub(super) fn is_ansible_inventory_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':'))
}
