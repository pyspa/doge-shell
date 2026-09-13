//! Reading this family's inventory out of the local system: the parsers for
//! each tool's output shape (nft, lvm, ufw, iw, audit, snapper JSON) and the
//! loaders for the files and directories Linux keeps that inventory in.
use super::*;

pub(super) fn selected_snapper_config(parsed_command_line: &ParsedCommandLine) -> Option<&str> {
    let words = completion_words(parsed_command_line);
    for (index, word) in words.iter().enumerate() {
        if matches!(*word, "-c" | "--config")
            && let Some(value) = words
                .get(index + 1)
                .copied()
                .filter(|value| !value.is_empty())
        {
            return Some(value);
        }
        if let Some(value) = word
            .strip_prefix("--config=")
            .filter(|value| !value.is_empty())
        {
            return Some(value);
        }
        if let Some(value) = word.strip_prefix("-c").filter(|value| !value.is_empty()) {
            return Some(value);
        }
    }
    None
}

pub(super) fn snapper_snapshot_filter(current_token: &str) -> (Option<String>, String) {
    if let Some((left, right)) = current_token.split_once("..")
        && !left.is_empty()
        && left.chars().all(|character| character.is_ascii_digit())
        && right.chars().all(|character| character.is_ascii_digit())
    {
        return (Some(format!("{left}..")), right.to_string());
    }

    if let Some((left, right)) = current_token.split_once('-')
        && !left.is_empty()
        && left.chars().all(|character| character.is_ascii_digit())
        && right.chars().all(|character| character.is_ascii_digit())
    {
        return (Some(format!("{left}-")), right.to_string());
    }

    (None, current_token.to_string())
}

pub(super) fn load_file_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    dedup_sorted(
        entries
            .flatten()
            .filter(|entry| entry.path().is_file())
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .collect(),
    )
}

pub(super) fn load_file_stems(dir: &Path, suffix: &str) -> Vec<String> {
    dedup_sorted(
        load_file_names(dir)
            .into_iter()
            .filter_map(|name| name.strip_suffix(suffix).map(str::to_string))
            .filter(|name| !name.is_empty())
            .collect(),
    )
}

pub(super) fn parse_snapper_snapshot_json(output: &str) -> Vec<String> {
    fn collect_numbers(value: &serde_json::Value, values: &mut Vec<String>) {
        match value {
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_numbers(item, values);
                }
            }
            serde_json::Value::Object(object) => {
                if let Some(number) = object.get("number") {
                    match number {
                        serde_json::Value::Number(number) => values.push(number.to_string()),
                        serde_json::Value::String(number) if !number.is_empty() => {
                            values.push(number.clone())
                        }
                        _ => {}
                    }
                }
                for value in object.values() {
                    if !matches!(
                        value,
                        serde_json::Value::Number(_) | serde_json::Value::String(_)
                    ) {
                        collect_numbers(value, values);
                    }
                }
            }
            _ => {}
        }
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    collect_numbers(&value, &mut values);
    dedup_sorted(values)
}

/// Extracts interface names from `iw dev`, whose device rows are indented
/// under each `phy#N` block as `Interface wlan0`.
pub(super) fn parse_iw_devices(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.trim().strip_prefix("Interface "))
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect(),
    )
}

pub(super) fn load_login_shells(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    dedup_sorted(
        contents
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('/'))
            .map(str::to_string)
            .collect(),
    )
}

pub(super) fn load_udev_subsystems(path: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    dedup_sorted(
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect(),
    )
}

/// Extracts boolean names from `getsebool -a`, which prints `name --> on`.
pub(super) fn parse_selinux_booleans(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split("-->").next())
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect(),
    )
}

pub(super) fn load_ip_route_tables(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return vec![
            "default".to_string(),
            "main".to_string(),
            "local".to_string(),
        ];
    };
    let mut values = vec![
        "default".to_string(),
        "main".to_string(),
        "local".to_string(),
    ];
    for line in contents.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let _id = parts.next();
        if let Some(name) = parts.next()
            && is_simple_completion_value(name)
        {
            values.push(name.to_string());
        }
    }
    dedup_sorted(values)
}

pub(super) fn parse_nft_tables(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                (parts.next()? == "table").then_some(())?;
                let _family = parts.next()?;
                parts.next().map(str::to_string)
            })
            .collect(),
    )
}

pub(super) fn parse_nft_chains(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                (parts.next()? == "chain").then_some(())?;
                parts.next().map(str::to_string)
            })
            .collect(),
    )
}

pub(super) fn parse_first_column_values(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split_whitespace().next().map(str::to_string))
            .collect(),
    )
}

pub(super) fn parse_ufw_applications(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.ends_with(':'))
            .map(str::to_string)
            .collect(),
    )
}

pub(super) fn parse_lvm_logical_volumes(lines: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    for line in lines {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if let Some(path) = fields.first()
            && path.starts_with('/')
        {
            values.push((*path).to_string());
        }
        if fields.len() >= 3 {
            values.push(format!("{}/{}", fields[1], fields[2]));
            values.push(fields[2].to_string());
        }
    }
    dedup_sorted(values)
}

pub(super) fn parse_btrfs_subvolumes(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                line.split_once(" path ")
                    .map(|(_, path)| path.trim().to_string())
            })
            .collect(),
    )
}

pub(super) fn load_mdadm_arrays(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for line in contents.lines() {
        let Some((name, _rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.starts_with("md") && is_simple_completion_value(name) {
            values.push(name.to_string());
            values.push(format!("/dev/{name}"));
        }
    }
    dedup_sorted(values)
}

pub(super) fn load_dev_mapper_devices(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name == "control" || !is_simple_completion_value(&name) {
            continue;
        }
        values.push(name.clone());
        values.push(format!("/dev/mapper/{name}"));
    }
    dedup_sorted(values)
}

pub(super) fn load_audit_rule_keys(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rules") {
            continue;
        }
        let Ok(contents) = fs::read_to_string(path) else {
            continue;
        };
        values.extend(parse_audit_rule_keys(&contents));
    }
    dedup_sorted(values)
}

pub(super) fn parse_audit_rule_keys(contents: &str) -> Vec<String> {
    let mut values = Vec::new();
    for line in contents.lines() {
        let mut parts = line.split_whitespace().peekable();
        while let Some(part) = parts.next() {
            if part == "-k" {
                if let Some(value) = parts.peek()
                    && is_simple_completion_value(value)
                {
                    values.push((*value).to_string());
                }
            } else if let Some(value) = part.strip_prefix("-k") {
                if is_simple_completion_value(value) {
                    values.push(value.to_string());
                }
            } else if let Some(value) = part.strip_prefix("key=")
                && is_simple_completion_value(value)
            {
                values.push(value.to_string());
            }
        }
    }
    dedup_sorted(values)
}

pub(super) fn load_selinux_module_files(root: &Path) -> Vec<String> {
    let mut values = Vec::new();
    collect_selinux_module_files(root, 0, &mut values);
    dedup_sorted(values)
}

pub(super) fn collect_selinux_module_files(dir: &Path, depth: usize, values: &mut Vec<String>) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_selinux_module_files(&path, depth + 1, values);
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("cil" | "pp")
        ) && is_simple_completion_value(stem)
        {
            values.push(stem.to_string());
        }
    }
}

pub(super) fn is_simple_completion_value(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
}
