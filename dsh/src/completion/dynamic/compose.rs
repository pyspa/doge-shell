//! Working out which Docker Compose project a command line is talking about:
//! finding the compose file (from `-f`, from the `docker compose` subcommand's
//! own options, or by walking up from the cwd) and reading its service names.
use super::*;

pub(super) fn find_compose_file(current_dir: &Path) -> Option<PathBuf> {
    const CANDIDATES: [&str; 4] = [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
    ];

    current_dir.ancestors().find_map(|dir| {
        CANDIDATES
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.exists())
    })
}

pub(super) fn selected_docker_compose_command(
    parsed_command_line: &ParsedCommandLine,
) -> Option<&str> {
    let mut skip_next_value = false;

    for token in docker_compose_words(parsed_command_line) {
        if skip_next_value {
            skip_next_value = false;
            continue;
        }

        if docker_compose_option_takes_value(token) {
            skip_next_value = true;
            continue;
        }

        if is_inline_docker_compose_option_value(token) || token.starts_with('-') {
            continue;
        }

        return Some(token);
    }

    None
}

pub(super) fn selected_docker_compose_file(
    parsed_command_line: &ParsedCommandLine,
    current_dir: &Path,
) -> Option<PathBuf> {
    let words = docker_compose_words(parsed_command_line);

    for (index, token) in words.iter().enumerate() {
        if *token == "-f" || *token == "--file" {
            let Some(value) = words.get(index + 1).copied() else {
                continue;
            };
            return compose_file_path_from_token(current_dir, value);
        }

        if let Some(value) = token
            .strip_prefix("--file=")
            .or_else(|| token.strip_prefix("-f="))
        {
            return compose_file_path_from_token(current_dir, value);
        }
    }

    None
}

pub(super) fn docker_compose_words(parsed_command_line: &ParsedCommandLine) -> Vec<&str> {
    let words = completion_words(parsed_command_line);
    if parsed_command_line.command == "docker-compose" {
        return words;
    }

    if parsed_command_line.command == "docker" {
        let mut skip_next_value = false;
        let mut compose_index = None;
        for (index, word) in words.iter().enumerate() {
            if skip_next_value {
                skip_next_value = false;
                continue;
            }
            if docker_global_option_takes_value(word) {
                skip_next_value = true;
                continue;
            }
            if is_inline_docker_global_option_value(word) || word.starts_with('-') {
                continue;
            }
            if *word == "compose" {
                compose_index = Some(index);
                break;
            }
        }
        if let Some(index) = compose_index {
            return words.into_iter().skip(index + 1).collect();
        }
    }

    Vec::new()
}

pub(super) fn compose_file_path_from_token(current_dir: &Path, token: &str) -> Option<PathBuf> {
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

pub(super) fn docker_compose_option_takes_value(token: &str) -> bool {
    matches!(
        token,
        "-f" | "--file"
            | "-p"
            | "--project-name"
            | "--profile"
            | "--env-file"
            | "--project-directory"
            | "--parallel"
    )
}

pub(super) fn docker_global_option_takes_value(token: &str) -> bool {
    matches!(
        token,
        "-c" | "--config"
            | "--context"
            | "-H"
            | "--host"
            | "--log-level"
            | "--tlscacert"
            | "--tlscert"
            | "--tlskey"
    )
}

pub(super) fn is_inline_docker_global_option_value(token: &str) -> bool {
    token.starts_with("--config=")
        || token.starts_with("-c=")
        || token.starts_with("--context=")
        || token.starts_with("-H=")
        || token.starts_with("--host=")
        || token.starts_with("--log-level=")
        || token.starts_with("--tlscacert=")
        || token.starts_with("--tlscert=")
        || token.starts_with("--tlskey=")
}

pub(super) fn is_inline_docker_compose_option_value(token: &str) -> bool {
    token.starts_with("--file=")
        || token.starts_with("-f=")
        || token.starts_with("--project-name=")
        || token.starts_with("--profile=")
        || token.starts_with("--env-file=")
        || token.starts_with("--project-directory=")
        || token.starts_with("--parallel=")
}

pub(super) fn parse_compose_service_names(path: &Path) -> Result<Vec<String>> {
    let contents = fs::read_to_string(path)?;
    let mut in_services = false;
    let mut services_indent = 0usize;
    let mut service_indent = None;
    let mut names = Vec::new();

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.chars().take_while(|c| c.is_whitespace()).count();
        if !in_services {
            if trimmed == "services:" {
                in_services = true;
                services_indent = indent;
            }
            continue;
        }

        if indent <= services_indent {
            break;
        }

        if trimmed.starts_with('-') {
            continue;
        }

        if !trimmed.ends_with(':') {
            continue;
        }

        let key = trimmed.trim_end_matches(':').trim();
        if key.is_empty() || key.contains(' ') {
            continue;
        }

        match service_indent {
            None => {
                service_indent = Some(indent);
                names.push(key.to_string());
            }
            Some(expected_indent) if indent == expected_indent => names.push(key.to_string()),
            _ => {}
        }
    }

    let mut seen = HashSet::new();
    names.retain(|name| seen.insert(name.clone()));
    Ok(names)
}
