//! Secret management module
//!
//! Manages detection of sensitive information, redaction, and history skipping.

use once_cell::sync::Lazy;
use parking_lot::RwLock;
use regex::Regex;
use std::collections::{HashMap, HashSet};

/// Secret history exclusion mode
#[derive(Debug, Clone, PartialEq, Default)]
pub enum SecretHistoryMode {
    /// Do not save commands containing secrets to history (default)
    #[default]
    Skip,
    /// Save with secret parts redacted
    Redact,
    /// No filtering (traditional behavior)
    None,
}

impl std::str::FromStr for SecretHistoryMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "skip" => Ok(SecretHistoryMode::Skip),
            "redact" => Ok(SecretHistoryMode::Redact),
            "none" => Ok(SecretHistoryMode::None),
            _ => Err(format!(
                "Invalid history mode: {}. Valid values: skip, redact, none",
                s
            )),
        }
    }
}

/// Default secret keyword patterns
static DEFAULT_SENSITIVE_KEYWORDS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
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
    .into_iter()
    .collect()
});

/// Command patterns containing secret values (e.g., export API_KEY=xxx, VAR=value command)
static ASSIGNMENT_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b([A-Z_][A-Z0-9_]*)=(\S+)").unwrap());

/// Secret management
#[derive(Debug)]
pub struct SecretManager {
    /// Custom secret patterns (regex)
    custom_patterns: RwLock<Vec<Regex>>,
    /// Session-specific secrets
    session_secrets: RwLock<HashMap<String, String>>,
    /// History exclusion mode
    history_mode: RwLock<SecretHistoryMode>,
    /// Additional secret keywords
    additional_keywords: RwLock<HashSet<String>>,
}

impl Default for SecretManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretManager {
    pub fn new() -> Self {
        SecretManager {
            custom_patterns: RwLock::new(Vec::new()),
            session_secrets: RwLock::new(HashMap::new()),
            history_mode: RwLock::new(SecretHistoryMode::default()),
            additional_keywords: RwLock::new(HashSet::new()),
        }
    }

    /// Check if the key name is sensitive
    pub fn is_sensitive_key(&self, key: &str) -> bool {
        let key_upper = key.to_ascii_uppercase();

        // Check default keywords
        for keyword in DEFAULT_SENSITIVE_KEYWORDS.iter() {
            if key_upper.contains(keyword) {
                return true;
            }
        }

        // Check additional keywords
        let additional = self.additional_keywords.read();
        for keyword in additional.iter() {
            if key_upper.contains(&keyword.to_ascii_uppercase()) {
                return true;
            }
        }

        // Check custom patterns
        let patterns = self.custom_patterns.read();
        for pattern in patterns.iter() {
            if pattern.is_match(key) {
                return true;
            }
        }

        false
    }

    /// Check if the command contains secrets
    pub fn is_sensitive_command(&self, cmd: &str) -> bool {
        // Check assignment patterns (VAR=value)
        for cap in ASSIGNMENT_PATTERN.captures_iter(cmd) {
            if let Some(var_name) = cap.get(1)
                && self.is_sensitive_key(var_name.as_str())
            {
                return true;
            }
        }

        // Check entire command with custom patterns
        let patterns = self.custom_patterns.read();
        for pattern in patterns.iter() {
            if pattern.is_match(cmd) {
                return true;
            }
        }

        false
    }

    /// Redact secret parts and return
    pub fn redact_command(&self, cmd: &str) -> String {
        let mut result = cmd.to_string();

        // Redact assignment patterns
        for cap in ASSIGNMENT_PATTERN.captures_iter(cmd) {
            if let (Some(var_match), Some(val_match)) = (cap.get(1), cap.get(2))
                && self.is_sensitive_key(var_match.as_str())
            {
                let original = format!("{}={}", var_match.as_str(), val_match.as_str());
                let redacted = format!("{}=***", var_match.as_str());
                result = result.replace(&original, &redacted);
            }
        }

        result
    }

    /// Determine if it should be saved to history, and redact if necessary
    pub fn process_for_history(&self, cmd: &str) -> Option<String> {
        let mode = self.history_mode.read().clone();

        match mode {
            SecretHistoryMode::None => Some(cmd.to_string()),
            SecretHistoryMode::Skip => {
                if self.is_sensitive_command(cmd) {
                    None
                } else {
                    Some(cmd.to_string())
                }
            }
            SecretHistoryMode::Redact => {
                if self.is_sensitive_command(cmd) {
                    Some(self.redact_command(cmd))
                } else {
                    Some(cmd.to_string())
                }
            }
        }
    }

    /// Add custom pattern
    pub fn add_pattern(&self, pattern: &str) -> Result<(), String> {
        let regex = Regex::new(pattern).map_err(|e| format!("Invalid regex: {}", e))?;
        self.custom_patterns.write().push(regex);
        Ok(())
    }

    /// Register additional keyword
    pub fn add_keyword(&self, keyword: &str) {
        self.additional_keywords.write().insert(keyword.to_string());
    }

    /// Get list of registered patterns
    pub fn list_patterns(&self) -> Vec<String> {
        self.custom_patterns
            .read()
            .iter()
            .map(|r| r.as_str().to_string())
            .collect()
    }

    /// Set history mode
    pub fn set_history_mode(&self, mode: SecretHistoryMode) {
        *self.history_mode.write() = mode;
    }

    /// Get current history mode
    pub fn history_mode(&self) -> SecretHistoryMode {
        self.history_mode.read().clone()
    }

    /// Set session-specific secret
    pub fn set_session_secret(&self, key: &str, value: &str) {
        self.session_secrets
            .write()
            .insert(key.to_string(), value.to_string());
    }

    /// Get session-specific secret
    pub fn get_session_secret(&self, key: &str) -> Option<String> {
        self.session_secrets.read().get(key).cloned()
    }

    /// Remove session-specific secret
    pub fn remove_session_secret(&self, key: &str) -> Option<String> {
        self.session_secrets.write().remove(key)
    }

    /// Clear all session secrets
    pub fn clear_session_secrets(&self) {
        self.session_secrets.write().clear();
    }

    /// Get list of session secret keys
    pub fn list_session_secret_keys(&self) -> Vec<String> {
        self.session_secrets.read().keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_sensitive_key_defaults() {
        let manager = SecretManager::new();

        // Keys that should match default patterns
        assert!(manager.is_sensitive_key("API_KEY"));
        assert!(manager.is_sensitive_key("MY_API_KEY"));
        assert!(manager.is_sensitive_key("GITHUB_TOKEN"));
        assert!(manager.is_sensitive_key("DB_PASSWORD"));
        assert!(manager.is_sensitive_key("AWS_SECRET_KEY"));
        assert!(manager.is_sensitive_key("AUTH_HEADER"));

        // Keys that should not match
        assert!(!manager.is_sensitive_key("HOME"));
        assert!(!manager.is_sensitive_key("PATH"));
        assert!(!manager.is_sensitive_key("EDITOR"));
    }

    #[test]
    fn test_is_sensitive_command() {
        let manager = SecretManager::new();

        // Commands containing secrets (assignment pattern)
        assert!(manager.is_sensitive_command("export API_KEY=abc123"));
        assert!(manager.is_sensitive_command("DB_PASSWORD=secret ./run.sh"));
        assert!(manager.is_sensitive_command("GITHUB_TOKEN=ghp_xxx git push"));

        // Commands NOT containing secrets
        assert!(!manager.is_sensitive_command("ls -la"));
        assert!(!manager.is_sensitive_command("echo hello"));
        assert!(!manager.is_sensitive_command("export HOME=/home/user"));
        // curl headers are not assignment patterns, so not detected (future custom pattern support)
        assert!(!manager.is_sensitive_command("curl -H 'Authorization: Bearer xxx' URL"));
    }

    #[test]
    fn test_redact_command() {
        let manager = SecretManager::new();

        let cmd = "export API_KEY=abc123 DB_PASSWORD=secret";
        let redacted = manager.redact_command(cmd);
        assert!(redacted.contains("API_KEY=***"));
        assert!(redacted.contains("DB_PASSWORD=***"));
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("secret"));
    }

    #[test]
    fn test_history_mode_skip() {
        let manager = SecretManager::new();
        manager.set_history_mode(SecretHistoryMode::Skip);

        // Commands containing secrets return None
        assert!(
            manager
                .process_for_history("export API_KEY=secret")
                .is_none()
        );

        // Commands NOT containing secrets remain as is
        let result = manager.process_for_history("ls -la");
        assert_eq!(result, Some("ls -la".to_string()));
    }

    #[test]
    fn test_history_mode_redact() {
        let manager = SecretManager::new();
        manager.set_history_mode(SecretHistoryMode::Redact);

        let result = manager.process_for_history("export API_KEY=secret");
        assert!(result.is_some());
        let cmd = result.unwrap();
        assert!(cmd.contains("API_KEY=***"));
        assert!(!cmd.contains("secret"));
    }

    #[test]
    fn test_history_mode_none() {
        let manager = SecretManager::new();
        manager.set_history_mode(SecretHistoryMode::None);

        // No filtering
        let result = manager.process_for_history("export API_KEY=secret");
        assert_eq!(result, Some("export API_KEY=secret".to_string()));
    }

    #[test]
    fn test_custom_pattern() {
        let manager = SecretManager::new();
        manager.add_pattern("MY_CUSTOM_.*").unwrap();

        assert!(manager.is_sensitive_key("MY_CUSTOM_VAR"));
        assert!(manager.is_sensitive_key("MY_CUSTOM_123"));
    }

    #[test]
    fn test_session_secrets() {
        let manager = SecretManager::new();

        // Set and get
        manager.set_session_secret("DB_PASS", "secret123");
        assert_eq!(
            manager.get_session_secret("DB_PASS"),
            Some("secret123".to_string())
        );

        // Non-existent key
        assert!(manager.get_session_secret("NONEXISTENT").is_none());

        // Remove
        manager.remove_session_secret("DB_PASS");
        assert!(manager.get_session_secret("DB_PASS").is_none());
    }
}
