//! Recognising and hiding secrets in text the shell is about to show, log or
//! hand to a model: which environment-variable names count as sensitive, which
//! filesystem paths hold credentials, and the regex-driven redaction that
//! rewrites assignments, bearer tokens, query parameters and PEM blocks.
use super::*;

// Enumerate keys independently of values: a non-sensitive quoted assignment
// must not swallow a sensitive assignment nested inside its text.
static ASSIGNMENT_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b([A-Z_][A-Z0-9_]*)=").expect("assignment key pattern"));

const SECRET_OPTION_NAMES: &str =
    r"password|passwd|passphrase|token|secret|api[-_]?key|access[-_]?token";

static SECRET_OPTION_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)--?(?:{SECRET_OPTION_NAMES})(?:\s+|=)|-p\s+"))
        .expect("secret option prefix")
});

static MULTILINE_SECRET_PREFIX: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)authorization|--?(?:{SECRET_OPTION_NAMES})|-p"
    ))
    .ok()
});

/// A conservative streaming boundary check, using the same option names and
/// Unicode case folding as the canonical redactor. False positives are safe:
/// option whitespace/quoted values and Authorization whitespace may span lines.
pub fn may_redact_across_lines(text: &str) -> bool {
    let last_value_delimiter = text.rfind(['"', '\'', '\\']);
    MULTILINE_SECRET_PREFIX
        .as_ref()
        .is_none_or(|pattern| pattern.is_match(text))
        || PRIVATE_KEY_MARKER
            .as_ref()
            .is_none_or(|pattern| pattern.is_match(text))
        || ASSIGNMENT_KEY.captures_iter(text).any(|key| {
            is_sensitive_key(&key[1])
                && last_value_delimiter.is_some_and(|last| key.get(0).expect("key").end() <= last)
        })
}

static AUTH_BEARER_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)authorization\s*:\s*bearer\s+").expect("bearer prefix"));

static QUERY_SECRET: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?i)([?&](?:token|access_token|api_key|apikey|auth|password)=)([^&\s]+)"#).ok()
});

static PRIVATE_KEY_MARKER: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").ok());

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

pub fn is_subscription_auth_path(path: &Path) -> bool {
    has_path_component(path, "subscription-auth")
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
        || is_subscription_auth_path(path)
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
    !sensitive_ranges(text, is_key_sensitive).is_empty()
}

pub fn redact_sensitive_text(text: &str) -> String {
    redact_sensitive_text_with(text, is_sensitive_key)
}

pub fn redact_sensitive_text_with<F>(text: &str, is_key_sensitive: F) -> String
where
    F: Fn(&str) -> bool,
{
    render_ranges(text, sensitive_ranges(text, is_key_sensitive))
}

fn sensitive_ranges<F: Fn(&str) -> bool>(
    text: &str,
    is_key_sensitive: F,
) -> Vec<std::ops::Range<usize>> {
    let values = std::cell::OnceCell::new();
    let mut ranges = Vec::new();
    for key in ASSIGNMENT_KEY.captures_iter(text) {
        if is_key_sensitive(&key[1])
            && let Some(range) = values
                .get_or_init(|| ValueIndex::new(text))
                .value(key.get(0).expect("key").end())
        {
            ranges.push(range);
        }
    }
    for prefix in SECRET_OPTION_PREFIX.find_iter(text) {
        if let Some(range) = values
            .get_or_init(|| ValueIndex::new(text))
            .value(prefix.end())
        {
            ranges.push(range);
        }
    }
    for prefix in AUTH_BEARER_PREFIX.find_iter(text) {
        let start = prefix.end();
        let len = text.as_bytes()[start..]
            .iter()
            .take_while(|byte| byte.is_ascii_alphanumeric() || b"._~+/=-".contains(byte))
            .count();
        if len > 0 {
            ranges.push(start..start + len);
        }
    }
    if let Some(pattern) = QUERY_SECRET.as_ref() {
        ranges.extend(
            pattern
                .captures_iter(text)
                .filter_map(|cap| cap.get(2).map(|value| value.range())),
        );
    }
    if let Some(pattern) = PRIVATE_KEY_MARKER.as_ref() {
        let mut covered = 0;
        for marker in pattern.find_iter(text) {
            if marker.start() < covered {
                continue;
            }
            // A missing END marker is an incomplete private key, not permission
            // to print its body. Keep the rest hidden through EOF.
            let tail = &text[marker.end()..];
            let mut end = tail
                .find("-----END ")
                .and_then(|at| {
                    tail[at..]
                        .find("PRIVATE KEY-----")
                        .map(|length| marker.end() + at + length + "PRIVATE KEY-----".len())
                })
                .unwrap_or(text.len());
            if pattern
                .find_at(text, marker.end())
                .is_some_and(|next| next.start() < end)
            {
                // Nested or overlapping private-key markers are ambiguous.
                // Withhold through EOF rather than exposing an inner key tail.
                end = text.len();
            }
            ranges.push(marker.start()..end);
            covered = end;
            if end == text.len() {
                break;
            }
        }
    }
    ranges
}

// Compute each original suffix's word end once. Quoted/unquoted segments
// concatenate, and escaped whitespace belongs to the same credential. This
// avoids rescanning long values for overlapping/nested key prefixes.
struct ValueIndex<'a> {
    text: &'a str,
    ends: Vec<usize>,
}
impl<'a> ValueIndex<'a> {
    fn new(text: &'a str) -> Self {
        let mut ends = vec![text.len(); text.len() + 1];
        let mut quotes = [None, None];
        for (at, ch) in text.char_indices().rev() {
            let next = at + ch.len_utf8();
            ends[at] = match ch {
                '\'' | '"' => {
                    let kind = usize::from(ch == '"');
                    let end = quotes[kind].map_or(text.len(), |close: usize| ends[close + 1]);
                    let slashes = text.as_bytes()[..at]
                        .iter()
                        .rev()
                        .take_while(|byte| **byte == b'\\')
                        .count();
                    if slashes % 2 == 0 {
                        quotes[kind] = Some(at);
                    }
                    end
                }
                '\\' => text[next..]
                    .chars()
                    .next()
                    .map_or(text.len(), |escaped| ends[next + escaped.len_utf8()]),
                ch if ch.is_whitespace() => at,
                _ => ends[next],
            };
        }
        Self { text, ends }
    }
    fn value(&self, start: usize) -> Option<std::ops::Range<usize>> {
        if start == self.text.len() || self.text[start..].starts_with(char::is_whitespace) {
            None
        } else {
            Some(start..self.ends[start])
        }
    }
}

fn render_ranges(text: &str, mut ranges: Vec<std::ops::Range<usize>>) -> String {
    ranges.sort_unstable_by_key(|range| (range.start, range.end));
    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    let mut output = String::new();
    let mut at = 0;
    for range in merged {
        output.push_str(&text[at..range.start]);
        output.push_str("***");
        at = range.end;
    }
    output.push_str(&text[at..]);
    output
}

pub fn mask_env_value(key: &str, value: &str) -> String {
    if is_sensitive_key(key) || contains_sensitive_text(value) {
        "***".to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod subscription_tests {
    #[test]
    fn subscription_auth_records_are_excluded_from_model_file_tools() {
        for file in [
            "registration.json",
            "credentials-account.json",
            "session.lock",
            ".credentials-temp.tmp",
        ] {
            assert!(super::is_sensitive_path(
                &std::path::Path::new("/xdg/dogesh/subscription-auth").join(file)
            ));
        }
    }
}

/// Redact known credentials and generic shapes together without rewriting the
/// input before either detector has inspected it.
pub struct RedactedText {
    pub text: String,
    pub redacted: bool,
}

pub fn redact_sensitive_text_with_known(text: &str, known: &[String]) -> RedactedText {
    let mut ranges = sensitive_ranges(text, is_sensitive_key);
    for secret in known.iter().filter(|secret| !secret.is_empty()) {
        let mut from = 0;
        while let Some(offset) = text[from..].find(secret) {
            let start = from + offset;
            ranges.push(start..start + secret.len());
            // Advance one original character, not the whole match: known
            // credentials may overlap themselves or another credential.
            from = start
                + text[start..]
                    .chars()
                    .next()
                    .expect("matched text")
                    .len_utf8();
        }
    }
    let redacted = !ranges.is_empty();
    RedactedText {
        text: render_ranges(text, ranges),
        redacted,
    }
}

#[cfg(test)]
mod original_span_tests {
    use super::*;

    #[test]
    fn known_and_generic_spans_cannot_hide_each_others_matches() {
        for (known, input) in [
            ("TOKEN", "TOKEN=private-dummy-value"),
            ("Authorization", "Authorization: Bearer private-dummy-value"),
            ("secret", "--secret private-dummy-value"),
            ("API_KEY=known", "API_KEY=known-private-dummy-value"),
            ("private-dummy", "--token 'private-dummy-value'"),
        ] {
            let output = redact_sensitive_text_with_known(input, &[known.into()]).text;
            assert!(!output.contains(known), "{output}");
            assert!(!output.contains("private-dummy-value"), "{output}");
        }
        assert!(redact_sensitive_text_with_known("***", &["***".into()]).redacted);
        assert_eq!(
            redact_sensitive_text_with_known("ababa", &["aba".into()]).text,
            "***"
        );
        assert_eq!(
            redact_sensitive_text_with_known("あいうい", &["いう".into(), "うい".into()]).text,
            "あ***"
        );
    }

    #[test]
    fn whole_quoted_and_private_key_values_are_hidden_even_without_a_closer() {
        for input in [
            "TOKEN=\"private\nsecond value\" tail",
            r#"TOKEN="private\" second value" tail"#,
            r#"--token "private\" second value" tail"#,
            r#"PUBLIC="hello TOKEN='private second'""#,
            "PUBLIC=\"hello TOKEN='private\nsecond'\"",
            "TOKEN=prefixPASSWORD=\"private second\"",
            "--token prefix--token='private second'",
            "Authorization: Bearer Authorization: Bearer private-second",
            "TOKEN=private\\ second",
            "TOKEN=prefix\"private second\"",
            "--token prefix\"private second\"",
            "TOKEN='private'\"second\"",
            "-----BEGIN RSA PRIVATE KEY-----\n-----BEGIN EC PRIVATE KEY-----\n-----END RSA PRIVATE KEY-----\nprivate-second-body\n-----END EC PRIVATE KEY-----",
            "TOKEN=private\\\nsecond",
            "TOKEN='private second value",
            "--token \"private\nsecond value",
            "-----BEGIN RSA PRIVATE KEY-----\nprivate\nsecond\n-----END RSA PRIVATE KEY----- tail",
            "-----BEGIN PRIVATE KEY-----\nprivate\nsecond",
        ] {
            let output = redact_sensitive_text_with_known(input, &[]).text;
            assert!(!output.contains("private"), "{output}");
            assert!(!output.contains("second"), "{output}");
        }
    }
}
