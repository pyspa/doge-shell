//! Screening text that is about to reach a model: the prompt-injection pattern
//! table and the scan over it, plus the length-capped sanitiser applied to
//! anything forwarded as AI input.
use super::*;

impl SafetyGuard {
    /// Patterns that may indicate prompt injection attempts
    const INJECTION_PATTERNS: &'static [&'static str] = &[
        "ignore previous",
        "ignore all previous",
        "ignore the above",
        "disregard previous",
        "disregard all previous",
        "forget previous",
        "forget all previous",
        "forget your instructions",
        "override your instructions",
        "new instructions",
        "system prompt",
        "you are now",
        "act as if",
        "pretend you are",
        "jailbreak",
        "do anything now",
        "dan mode",
        "developer mode",
        "ignore safety",
        "bypass safety",
        "ignore security",
        "bypass security",
    ];

    /// Check if user input contains potential prompt injection patterns
    pub fn check_prompt_injection(input: &str) -> PromptInjectionResult {
        let input_lower = input.to_lowercase();
        let mut warnings = Vec::new();

        // Check for suspicious patterns
        for pattern in Self::INJECTION_PATTERNS {
            if input_lower.contains(pattern) {
                warnings.push(format!("Suspicious pattern detected: '{}'", pattern));
            }
        }

        // Check for excessive length (potential token flooding)
        if input.len() > 10000 {
            warnings.push(format!(
                "Input is very long ({} chars), may indicate injection attempt",
                input.len()
            ));
        }

        // Check for control characters (except common whitespace)
        let control_chars: Vec<char> = input
            .chars()
            .filter(|c| c.is_control() && *c != '\n' && *c != '\r' && *c != '\t')
            .collect();
        if !control_chars.is_empty() {
            warnings.push("Input contains control characters".to_string());
        }

        // Check for unusual Unicode that might be used for obfuscation
        let unusual_unicode = input.chars().any(|c| {
            matches!(c,
                '\u{200B}'..='\u{200F}' | // Zero-width chars
                '\u{2028}'..='\u{2029}' | // Line/paragraph separators
                '\u{202A}'..='\u{202E}' | // Directional formatting
                '\u{2060}'..='\u{206F}'   // Word joiner, invisible separators
            )
        });
        if unusual_unicode {
            warnings.push(
                "Input contains unusual Unicode characters (possible obfuscation)".to_string(),
            );
        }

        if warnings.is_empty() {
            PromptInjectionResult::Safe
        } else {
            PromptInjectionResult::Suspicious(warnings)
        }
    }

    /// Sanitize user input before sending to AI
    pub fn sanitize_ai_input(input: &str, max_length: usize) -> String {
        let mut sanitized = input.to_string();

        // Remove control characters (except common whitespace)
        sanitized = sanitized
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\r' || *c == '\t')
            .collect();

        // Remove zero-width and invisible characters
        sanitized = sanitized
            .chars()
            .filter(|c| {
                !matches!(*c,
                    '\u{200B}'..='\u{200F}' |
                    '\u{2028}'..='\u{2029}' |
                    '\u{202A}'..='\u{202E}' |
                    '\u{2060}'..='\u{206F}'
                )
            })
            .collect();

        // Truncate if too long
        if sanitized.len() > max_length {
            let mut end = max_length;
            while end > 0 && !sanitized.is_char_boundary(end) {
                end -= 1;
            }
            sanitized.truncate(end);
            sanitized.push_str("...(truncated)");
        }

        sanitized
    }
}

/// Result of prompt injection check
#[derive(Debug, Clone, PartialEq)]
pub enum PromptInjectionResult {
    /// Input appears safe
    Safe,
    /// Input contains suspicious patterns
    Suspicious(Vec<String>),
}
