use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SafetyLevel {
    Strict,
    Normal,
    Loose,
}

impl SafetyLevel {
    pub fn from_env_value(value: Option<String>) -> Self {
        match value
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("strict") => Self::Strict,
            Some("loose") => Self::Loose,
            _ => Self::Normal,
        }
    }

    pub fn requires_confirmation_for_sensitive_access(self) -> bool {
        !matches!(self, Self::Loose)
    }
}

static SECRET_ASSIGNMENT: Lazy<Option<Regex>> =
    Lazy::new(|| Regex::new(r"(?i)\b([A-Z_][A-Z0-9_]*)=([^\s]+)").ok());

static SECRET_OPTION: Lazy<Option<Regex>> = Lazy::new(|| {
    Regex::new(
        r#"(?i)(--?(?:password|passwd|passphrase|token|secret|api[-_]?key|access[-_]?token)(?:\s+|=)|-p\s+)([^\s"']+|"[^"]*"|'[^']*')"#,
    )
    .ok()
});

static AUTH_BEARER: Lazy<Option<Regex>> =
    Lazy::new(|| Regex::new(r#"(?i)(authorization\s*:\s*bearer\s+)([A-Za-z0-9._~+/=-]+)"#).ok());

static QUERY_SECRET: Lazy<Option<Regex>> = Lazy::new(|| {
    Regex::new(r#"(?i)([?&](?:token|access_token|api_key|apikey|auth|password)=)([^&\s]+)"#).ok()
});

static PRIVATE_KEY_MARKER: Lazy<Option<Regex>> =
    Lazy::new(|| Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").ok());

pub fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    [
        "API_KEY",
        "_KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "PASSPHRASE",
        "AUTH",
        "COOKIE",
        "SESSION",
        "CREDENTIAL",
        "PRIVATE",
        "ACCESS_KEY",
        "SECRET_KEY",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

pub fn is_sensitive_path(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    file_name == ".env"
        || file_name.starts_with(".env.")
        || file_name.ends_with("_history")
        || file_name == "id_rsa"
        || file_name == "id_ed25519"
        || file_name.ends_with(".pem")
        || file_name.ends_with(".key")
        || has_path_component(path, ".ssh")
        || has_path_component_sequence(path, &[".aws", "credentials"])
        || has_path_component_sequence(path, &[".config", "gcloud"])
        || has_path_component(path, ".azure")
        || has_path_component(path, "credentials")
        || has_path_component(path, "secrets")
}

fn has_path_component(path: &Path, needle: &str) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|value| value.eq_ignore_ascii_case(needle))
    })
}

fn has_path_component_sequence(path: &Path, sequence: &[&str]) -> bool {
    if sequence.is_empty() {
        return false;
    }

    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    components
        .windows(sequence.len())
        .any(|window| window.iter().zip(sequence).all(|(a, b)| a.as_str() == *b))
}

pub fn contains_sensitive_text(text: &str) -> bool {
    contains_sensitive_text_with(text, is_sensitive_key)
}

pub fn contains_sensitive_text_with<F>(text: &str, is_key_sensitive: F) -> bool
where
    F: Fn(&str) -> bool,
{
    if SECRET_ASSIGNMENT.as_ref().is_some_and(|pattern| {
        pattern
            .captures_iter(text)
            .any(|cap| cap.get(1).is_some_and(|key| is_key_sensitive(key.as_str())))
    }) {
        return true;
    }

    SECRET_OPTION
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(text))
        || AUTH_BEARER
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(text))
        || QUERY_SECRET
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(text))
        || PRIVATE_KEY_MARKER
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(text))
}

pub fn redact_sensitive_text(text: &str) -> String {
    redact_sensitive_text_with(text, is_sensitive_key)
}

pub fn redact_sensitive_text_with<F>(text: &str, is_key_sensitive: F) -> String
where
    F: Fn(&str) -> bool,
{
    let mut redacted = text.to_string();

    if let Some(pattern) = SECRET_ASSIGNMENT.as_ref() {
        redacted = pattern
            .replace_all(&redacted, |caps: &regex::Captures<'_>| {
                let key = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                if is_key_sensitive(key) {
                    format!("{key}=***")
                } else {
                    caps.get(0)
                        .map(|m| m.as_str())
                        .unwrap_or_default()
                        .to_string()
                }
            })
            .to_string();
    }

    if let Some(pattern) = SECRET_OPTION.as_ref() {
        redacted = pattern
            .replace_all(&redacted, |caps: &regex::Captures<'_>| {
                format!("{}***", caps.get(1).map(|m| m.as_str()).unwrap_or(""))
            })
            .to_string();
    }

    if let Some(pattern) = AUTH_BEARER.as_ref() {
        redacted = pattern
            .replace_all(&redacted, |caps: &regex::Captures<'_>| {
                format!("{}***", caps.get(1).map(|m| m.as_str()).unwrap_or(""))
            })
            .to_string();
    }

    if let Some(pattern) = QUERY_SECRET.as_ref() {
        redacted = pattern
            .replace_all(&redacted, |caps: &regex::Captures<'_>| {
                format!("{}***", caps.get(1).map(|m| m.as_str()).unwrap_or(""))
            })
            .to_string();
    }

    if let Some(pattern) = PRIVATE_KEY_MARKER.as_ref() {
        redacted = pattern
            .replace_all(&redacted, "-----BEGIN *** PRIVATE KEY-----")
            .to_string();
    }

    redacted
}

pub fn mask_env_value(key: &str, value: &str) -> String {
    if is_sensitive_key(key) || contains_sensitive_text(value) {
        "***".to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_path_detects_common_secret_files() {
        assert!(is_sensitive_path(Path::new(".env")));
        assert!(is_sensitive_path(Path::new("/home/me/.ssh/id_ed25519")));
        assert!(is_sensitive_path(Path::new("/home/me/.aws/credentials")));
        assert!(is_sensitive_path(Path::new("/repo/secrets/token.txt")));
        assert!(is_sensitive_path(Path::new("/repo/credentials/token.json")));
        assert!(!is_sensitive_path(Path::new("src/main.rs")));
        assert!(!is_sensitive_path(Path::new("dsh/src/secrets.rs")));
        assert!(!is_sensitive_path(Path::new("src/credentials.rs")));
    }

    #[test]
    fn sensitive_text_redacts_common_secret_shapes() {
        let input =
            "API_KEY=abc curl --token qwe -H 'Authorization: Bearer xyz' https://x?token=123";
        let redacted = redact_sensitive_text(input);
        assert!(redacted.contains("API_KEY=***"));
        assert!(redacted.contains("--token ***"));
        assert!(redacted.contains("Authorization: Bearer ***"));
        assert!(redacted.contains("?token=***"));
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("qwe"));
    }

    #[test]
    fn sensitive_text_accepts_custom_key_detector() {
        assert!(contains_sensitive_text_with("CUSTOM=value", |key| key == "CUSTOM"));
        let redacted = redact_sensitive_text_with("CUSTOM=value HOME=/tmp", |key| key == "CUSTOM");
        assert!(redacted.contains("CUSTOM=***"));
        assert!(redacted.contains("HOME=/tmp"));
        assert!(!redacted.contains("CUSTOM=value"));
    }
}
