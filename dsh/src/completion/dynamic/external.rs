use super::super::integrated::{CandidateType, EnhancedCandidate, matches_prefix};
use super::super::shell_path::normalize_path_token;
use super::{
    DynamicCompletionProvider, ExternalCompletionCacheKey, ParsedCommandLine, canonicalize_path,
    run_external_completer_for_key,
};
use std::path::Path;
use tracing::warn;

pub(super) fn collect(
    collector: &super::DynamicCompletionProvider,
    request: &super::registry::DynamicProviderRequest<'_>,
) -> Option<Vec<EnhancedCandidate>> {
    use super::*;

    let provider = request.provider.as_str();
    let parsed_command_line = request.parsed_command_line;
    let current_dir = request.current_dir;
    let cached_only = request.cache_policy.is_cached_only();
    let current_token = parsed_command_line.current_token.as_str();

    Some(match provider {
        "brew.installed" => collector.collect_brew_installed_candidates(current_token, cached_only),
        "ollama.model" => collector.collect_ollama_model_candidates(current_token, cached_only),
        "apt.installed_package" => collector.collect_apt_installed_package_candidates(
            current_dir,
            current_token,
            parsed_command_line.command.as_str(),
            cached_only,
        ),
        "apk.installed_package" => collector.collect_apk_installed_package_candidates(
            current_dir,
            current_token,
            cached_only,
        ),
        "dnf.installed_package" => collector.collect_rpm_installed_package_candidates(
            current_dir,
            current_token,
            "dnf",
            cached_only,
        ),
        "rpm.installed_package" => collector.collect_rpm_installed_package_candidates(
            current_dir,
            current_token,
            "rpm",
            cached_only,
        ),
        "zypper.installed_package" => collector.collect_zypper_installed_package_candidates(
            current_dir,
            current_token,
            cached_only,
        ),
        "machinectl.machine" => {
            collector.collect_machinectl_machine_candidates(current_dir, current_token, cached_only)
        }
        "ufw.application" => {
            collector.collect_ufw_application_candidates(current_dir, current_token, cached_only)
        }
        "pacman.package" => match pacman_sync_mode(parsed_command_line) {
            Some(sync) => collector.collect_pacman_package_candidates(
                current_dir,
                current_token,
                sync,
                cached_only,
            ),
            None => Vec::new(),
        },
        "audit.rule_key" => collector.collect_audit_rule_key_candidates(current_token, cached_only),
        "ansible.inventory_host" => collector.collect_ansible_inventory_host_candidates(
            parsed_command_line,
            current_dir,
            current_token,
            cached_only,
        ),
        "aws.profile" => collector.collect_aws_profile_candidates(current_token, cached_only),
        "az.subscription" => {
            collector.collect_az_subscription_candidates(current_token, cached_only)
        }
        "gcloud.configuration" => {
            collector.collect_gcloud_configuration_candidates(current_token, cached_only)
        }
        "gcloud.project" => collector.collect_gcloud_project_candidates(current_token, cached_only),
        "btrfs.subvolume" => {
            collector.collect_btrfs_subvolume_candidates(current_dir, current_token, cached_only)
        }
        "dmsetup.device" => collector.collect_dmsetup_device_candidates(current_token, cached_only),
        "mdadm.array" => collector.collect_mdadm_array_candidates(current_token, cached_only),
        _ => {
            return platform::collect(
                collector,
                provider,
                parsed_command_line,
                current_dir,
                cached_only,
            );
        }
    })
}

impl DynamicCompletionProvider {
    pub(crate) fn collect_external_candidates(
        &self,
        current_dir: &Path,
        input: &str,
        cursor_pos: usize,
        parsed_command_line: &ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        let Some(command_template) = self
            .environment
            .read()
            .get_var("DSH_EXTERNAL_COMPLETER")
            .filter(|value| !value.trim().is_empty())
        else {
            return Vec::new();
        };

        let subcommand_path = parsed_command_line.subcommand_path.join(" ");
        let key = ExternalCompletionCacheKey {
            command_template: command_template.clone(),
            current_dir: canonicalize_path(current_dir),
            input: input.to_string(),
            cursor_pos,
            command: parsed_command_line.command.clone(),
            current_token: parsed_command_line.current_token.clone(),
            subcommand_path,
        };

        let loader_key = key.clone();
        match self
            .load_external_candidates(key, move || run_external_completer_for_key(&loader_key))
        {
            Ok(candidates) => candidates,
            Err(err) => {
                warn!("External completer failed: {}", err);
                Vec::new()
            }
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_line_parser_preserves_declared_priority_and_type() {
        let candidate = parse_line(
            r#"{"text":"alpha","description":"from tool","type":"subcommand","priority":321}"#,
            "al",
        )
        .unwrap();

        assert_eq!(candidate.text, "alpha");
        assert_eq!(candidate.description.as_deref(), Some("from tool"));
        assert_eq!(candidate.candidate_type, CandidateType::SubCommand);
        assert_eq!(candidate.priority, 321);
    }
}
