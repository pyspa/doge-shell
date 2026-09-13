//! AWS/gcloud/Azure CLI config file parsing: profile, configuration, and
//! subscription names read from each tool's own on-disk config format.
use super::*;

pub(super) fn aws_config_dir(home: &Option<String>) -> PathBuf {
    home.as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".aws")
}

pub(super) fn load_aws_profiles(config_file: &Path, credentials_file: &Path) -> Vec<String> {
    let mut values = Vec::new();
    values.extend(load_aws_profile_sections(config_file, true));
    values.extend(load_aws_profile_sections(credentials_file, false));
    dedup_sorted(values)
}

pub(super) fn load_aws_profile_sections(path: &Path, config_style: bool) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(parse_ini_section_name)
        .filter_map(|section| {
            if config_style {
                section
                    .strip_prefix("profile ")
                    .map(str::to_string)
                    .or_else(|| (section == "default").then_some(section))
            } else {
                Some(section)
            }
        })
        .collect()
}

pub(super) fn parse_ini_section_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let section = trimmed.strip_prefix('[')?.strip_suffix(']')?.trim();
    (!section.is_empty()).then_some(section.to_string())
}

pub(super) fn gcloud_config_dir(home: &Option<String>, explicit: Option<String>) -> PathBuf {
    explicit
        .map(PathBuf::from)
        .or_else(|| {
            home.as_ref()
                .map(|home| PathBuf::from(home).join(".config/gcloud"))
        })
        .unwrap_or_else(|| PathBuf::from(".config/gcloud"))
}

pub(super) fn load_gcloud_configurations(config_dir: &Path) -> Vec<String> {
    let configurations_dir = config_dir.join("configurations");
    let Ok(entries) = fs::read_dir(configurations_dir) else {
        return Vec::new();
    };
    dedup_sorted(
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter_map(|name| name.strip_prefix("config_").map(str::to_string))
            .collect(),
    )
}

pub(super) fn load_gcloud_projects(config_dir: &Path) -> Vec<String> {
    let mut values = Vec::new();
    let configurations_dir = config_dir.join("configurations");
    if let Ok(entries) = fs::read_dir(configurations_dir) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("config_"))
            {
                values.extend(load_gcloud_project_values(&entry.path()));
            }
        }
    }
    dedup_sorted(values)
}

pub(super) fn load_gcloud_project_values(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                return None;
            }
            let (key, value) = trimmed.split_once('=')?;
            (key.trim() == "project" && !value.trim().is_empty()).then(|| value.trim().to_string())
        })
        .collect()
}

pub(super) fn azure_config_dir(home: &Option<String>, explicit: Option<String>) -> PathBuf {
    explicit
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| PathBuf::from(home).join(".azure")))
        .unwrap_or_else(|| PathBuf::from(".azure"))
}

pub(super) fn load_az_subscriptions(profile_file: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(profile_file) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    if let Some(subscriptions) = value
        .get("subscriptions")
        .and_then(serde_json::Value::as_array)
    {
        for subscription in subscriptions {
            values.extend(subscription.get("id").and_then(serde_json::Value::as_str));
        }
    }
    dedup_sorted(values.into_iter().map(str::to_string).collect())
}
