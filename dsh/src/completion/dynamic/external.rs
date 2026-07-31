use super::super::integrated::{CandidateType, EnhancedCandidate, matches_prefix};
use super::super::shell_path::normalize_path_token;

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
