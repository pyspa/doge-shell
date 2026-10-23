//! Running a completion helper and turning its output into values: the
//! external/fish completer entry points, the thin `Command` wrappers the
//! collectors share, and the generic line shapes (first field, first column,
//! whitespace-separated, non-empty) those outputs come in.
use super::*;

pub(super) fn run_external_completer_for_key(
    key: &ExternalCompletionCacheKey,
) -> Result<Vec<EnhancedCandidate>> {
    let command = external_completer_command(key);
    let lines = collect_command_lines(command)?;
    Ok(lines
        .into_iter()
        .filter_map(|line| external::parse_line(&line, &key.current_token))
        .collect())
}

fn external_completer_command(key: &ExternalCompletionCacheKey) -> std::process::Command {
    let mut command = runner::shell_command(&key.command_template);
    command
        .current_dir(&key.current_dir)
        .env("DOGESH_COMPLETION_INPUT", &key.input)
        .env("DOGESH_COMPLETION_CURSOR", key.cursor_pos.to_string())
        .env("DOGESH_COMPLETION_COMMAND", &key.command)
        .env("DOGESH_COMPLETION_CURRENT_TOKEN", &key.current_token)
        .env("DOGESH_COMPLETION_SUBCOMMAND_PATH", &key.subcommand_path);

    command
}

pub(super) fn run_fish_completer_for_key(
    command_path: &str,
    key: &ExternalCompletionCacheKey,
) -> Result<Vec<EnhancedCandidate>> {
    let command = fish_completer_command(command_path, key);
    let lines = collect_command_lines(command)?;
    Ok(lines
        .into_iter()
        .filter_map(|line| external::parse_fish_line(&line, &key.current_token))
        .collect())
}

fn fish_completer_command(
    command_path: &str,
    key: &ExternalCompletionCacheKey,
) -> std::process::Command {
    let mut command = runner::command(command_path);
    command
        .arg("-c")
        .arg("complete -C \"$argv[1]\"")
        .arg("--")
        .arg(&key.input)
        .current_dir(&key.current_dir);

    command
}

pub(super) fn run_command_stdout(
    command_path: &str,
    args: &[&str],
    current_dir: &Path,
) -> Result<String> {
    let mut command = runner::command(command_path);
    command.args(args).current_dir(current_dir);
    runner::collect_stdout(command)
}

pub(super) fn run_command_lines(
    command_path: &str,
    args: &[&str],
    current_dir: &Path,
) -> Result<Vec<String>> {
    let mut command = runner::command(command_path);
    command.args(args).current_dir(current_dir);
    collect_command_lines(command)
}

pub(super) fn collect_command_lines(command: std::process::Command) -> Result<Vec<String>> {
    Ok(runner::collect_stdout(command)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

pub(super) fn dedup_sorted(mut values: Vec<String>) -> Vec<String> {
    values.retain(|value| !value.trim().is_empty());
    values.sort();
    values.dedup();
    values
}

pub(super) fn parse_non_empty_lines(lines: &[String]) -> Vec<String> {
    dedup_sorted(lines.iter().map(|line| line.trim().to_string()).collect())
}

pub(super) fn shell_state_candidates(
    values: Vec<String>,
    current_token: &str,
    description: &str,
) -> Vec<EnhancedCandidate> {
    dedup_sorted(values)
        .into_iter()
        .filter(|value| matches_prefix(current_token, value))
        .map(|value| EnhancedCandidate {
            text: value,
            description: Some(description.to_string()),
            candidate_type: CandidateType::Argument,
            priority: 140,
        })
        .collect()
}

pub(super) fn parse_first_fields(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split_whitespace().next().map(str::to_string))
            .collect(),
    )
}

pub(super) fn parse_first_column_lines(lines: &[String]) -> Vec<String> {
    parse_first_fields(lines)
        .into_iter()
        .filter(|value| !value.eq_ignore_ascii_case("name"))
        .collect()
}

pub(super) fn parse_minikube_profiles(output: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    let mut profiles = Vec::new();
    for group in ["valid", "invalid"] {
        let Some(entries) = value.get(group).and_then(serde_json::Value::as_array) else {
            continue;
        };
        for entry in entries {
            if let Some(name) = entry
                .get("Name")
                .or_else(|| entry.get("name"))
                .and_then(serde_json::Value::as_str)
            {
                profiles.push(name.to_string());
            }
        }
    }
    dedup_sorted(profiles)
}

pub(super) fn parse_whitespace_values(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .flat_map(|line| line.split_whitespace())
            .map(str::to_string)
            .collect(),
    )
}

pub(super) fn parse_journalctl_boots(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let offset = fields.next()?;
                if offset.parse::<i32>().is_ok() {
                    Some(offset.to_string())
                } else {
                    fields.next().map(str::to_string)
                }
            })
            .collect(),
    )
}

pub(super) fn parse_networkctl_links(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let first = fields.next()?;
                if first == "IDX" {
                    return None;
                }
                if first.parse::<u32>().is_ok() {
                    fields.next().map(str::to_string)
                } else {
                    Some(first.to_string())
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod command_contract_tests {
    use super::*;
    #[test]
    fn external_and_fish_preserve_full_input_and_character_cursor() {
        let input = "echo 日本語 && git checkout ma later";
        let cursor = "echo 日本語 && git checkout ma".chars().count();
        let parsed = crate::completion::parser::CommandLineParser::new().parse(
            input,
            crate::completion::command_segment::char_to_byte(input, cursor),
        );
        let key = ExternalCompletionCacheKey {
            command_template: "fixed-helper".into(),
            current_dir: std::env::temp_dir(),
            input: input.into(),
            cursor_pos: cursor,
            command: parsed.command,
            current_token: parsed.current_token,
            subcommand_path: parsed.subcommand_path.join(" "),
        };
        let external = external_completer_command(&key);
        let vars: std::collections::HashMap<_, _> = external
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert_eq!(vars["DOGESH_COMPLETION_INPUT"], input);
        assert_eq!(vars["DOGESH_COMPLETION_CURSOR"], cursor.to_string());
        assert_eq!(vars["DOGESH_COMPLETION_COMMAND"], "git");
        assert_eq!(vars["DOGESH_COMPLETION_CURRENT_TOKEN"], "ma");
        assert_eq!(external.get_current_dir(), Some(key.current_dir.as_path()));
        let mut fish_key = key;
        fish_key.input = input.chars().take(cursor).collect();
        let fish = fish_completer_command("fixed-fish", &fish_key);
        let args: Vec<_> = fish
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            ["-c", "complete -C \"$argv[1]\"", "--", &fish_key.input]
        );
    }
}
