//! Value parsers/root-finders for tools with too little surface area each to
//! earn their own file: `ffmpeg` codec/format tables, `mise` tool names,
//! `jj` (Jujutsu) repository selection, `meson` build directory resolution
//! and target names, and `golangci-lint` linter names.
use super::*;

/// Parses the tabular listings printed by `ffmpeg -encoders`, `-decoders` and
/// `-formats`. Every table starts after a row of dashes and then prints
/// `<flags> <name> <description>`, where multi-name formats are comma joined.
pub(super) fn parse_ffmpeg_table(lines: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    let mut in_table = false;
    for line in lines {
        let trimmed = line.trim();
        if !in_table {
            if !trimmed.is_empty() && trimmed.chars().all(|c| c == '-') {
                in_table = true;
            }
            continue;
        }
        let mut fields = trimmed.split_whitespace();
        let Some(_flags) = fields.next() else {
            continue;
        };
        let Some(names) = fields.next() else {
            continue;
        };
        values.extend(
            names
                .split(',')
                .filter(|name| !name.is_empty())
                .map(str::to_string),
        );
    }
    dedup_sorted(values)
}

/// Parses `mise ls --installed`, dropping the `Tool Version ...` header row.
pub(super) fn parse_mise_tools(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|name| !name.is_empty() && *name != "Tool")
            .map(str::to_string)
            .collect(),
    )
}

pub(super) fn find_jj_root(current_dir: &Path) -> Option<PathBuf> {
    current_dir
        .ancestors()
        .find(|candidate| candidate.join(".jj").exists())
        .map(Path::to_path_buf)
}

pub(super) fn selected_jj_repository(
    parsed_command_line: &ParsedCommandLine,
    current_dir: &Path,
) -> Option<PathBuf> {
    let words = completion_words(parsed_command_line);
    for (index, word) in words.iter().enumerate() {
        if matches!(*word, "-R" | "--repository")
            && let Some(value) = words
                .get(index + 1)
                .copied()
                .filter(|value| !value.is_empty())
        {
            return Some(resolve_command_path_token(current_dir, value));
        }
        if let Some(value) = word
            .strip_prefix("--repository=")
            .or_else(|| word.strip_prefix("-R="))
            .filter(|value| !value.is_empty())
        {
            return Some(resolve_command_path_token(current_dir, value));
        }
        if let Some(value) = word.strip_prefix("-R").filter(|value| !value.is_empty()) {
            return Some(resolve_command_path_token(current_dir, value));
        }
    }
    None
}

pub(super) fn selected_meson_build_dir(
    parsed_command_line: &ParsedCommandLine,
    project_root: &Path,
) -> PathBuf {
    let words = completion_words(parsed_command_line);
    for (index, word) in words.iter().enumerate() {
        if matches!(*word, "-C" | "--builddir")
            && let Some(value) = words
                .get(index + 1)
                .copied()
                .filter(|value| !value.is_empty())
        {
            return resolve_project_path(project_root, value);
        }
        if let Some(value) = word
            .strip_prefix("--builddir=")
            .filter(|value| !value.is_empty())
        {
            return resolve_project_path(project_root, value);
        }
    }

    ["build", "builddir", "_build"]
        .into_iter()
        .map(|name| project_root.join(name))
        .find(|path| path.is_dir())
        .unwrap_or_else(|| project_root.join("build"))
}

pub(super) fn parse_golangci_linters(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let line = line.trim().trim_start_matches(['-', '*', ' ']);
                let (name, _) = line.split_once(':')?;
                let name = name.trim();
                (!name.is_empty()
                    && name
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
                .then(|| name.to_string())
            })
            .collect(),
    )
}

pub(super) fn parse_meson_targets(output: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    let Some(targets) = value.as_array() else {
        return Vec::new();
    };
    dedup_sorted(
        targets
            .iter()
            .filter_map(|target| target.get("name").and_then(serde_json::Value::as_str))
            .map(str::to_string)
            .collect(),
    )
}

pub(super) fn dedup_sorted_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    paths
}
