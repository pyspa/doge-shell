//! Line and path shapes shared across this family's ecosystems: the four
//! command-output parsers the `LocalSpec` rows and collectors name, and the
//! path resolution that turns a token or marker into a concrete directory.
use super::*;

pub(super) fn parse_plain_lines(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

pub(super) fn parse_first_field_lines(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_string)
            .collect(),
    )
}

/// Parses `op item list --format json` (a JSON array of item objects).
pub(super) fn parse_op_items(lines: &[String]) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&lines.join("\n")) else {
        return Vec::new();
    };
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };
    dedup_sorted(
        entries
            .iter()
            .filter_map(|entry| entry.get("title").and_then(serde_json::Value::as_str))
            .filter(|title| !title.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Parses `name: description` style listings such as `bat --list-languages`
/// (`Rust:rs`) and `rg --type-list` (`rust: *.rs`).
pub(super) fn parse_colon_prefixed_names(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split_once(':'))
            .map(|(name, _)| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect(),
    )
}

pub(super) fn resolve_command_path_token(current_dir: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(normalize_path_token(value));
    if path.is_absolute() {
        path
    } else {
        current_dir.join(path)
    }
}

pub(super) fn resolve_project_path(project_root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

pub(super) fn find_ancestor_containing(current_dir: &Path, markers: &[&str]) -> Option<PathBuf> {
    let mut dir = Some(current_dir);
    while let Some(candidate) = dir {
        if markers
            .iter()
            .any(|marker| candidate.join(marker).is_file())
        {
            return Some(candidate.to_path_buf());
        }
        dir = candidate.parent();
    }
    None
}
