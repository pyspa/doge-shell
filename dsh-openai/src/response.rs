//! Helpers for reading model output.

use serde_json::{Value, json};

/// Ask the provider to guarantee a JSON object response.
///
/// Endpoints that do not understand `response_format` reject the request with a
/// 400; the client drops the field and retries once, so this is safe to send to
/// an arbitrary OpenAI-compatible server.
pub fn json_object_format() -> Value {
    json!({ "type": "json_object" })
}

/// Strip a markdown code fence from a model answer.
///
/// Models add fences even when told not to. Every caller used to re-implement
/// this, each handling a slightly different set of cases.
pub fn strip_code_fence(content: &str) -> String {
    let trimmed = content.trim();

    let Some(rest) = trimmed.strip_prefix("```") else {
        // No fence, but a bare-backtick answer still needs unwrapping.
        return trimmed.trim_matches('`').trim().to_string();
    };

    // Per CommonMark the rest of the opening fence line is the info string, so
    // drop it whatever it says - "```sh title=fix" is as valid as "```json".
    // Without a newline the fence is malformed; keep the remainder as content.
    let body = match rest.split_once('\n') {
        Some((_info, body)) => body,
        None => rest,
    };

    let body = body
        .strip_suffix("```")
        .unwrap_or_else(|| body.trim_end().strip_suffix("```").unwrap_or(body))
        .trim();

    // A degenerate fence such as "````" leaves nothing but backticks behind.
    if body.trim_matches('`').trim().is_empty() {
        return String::new();
    }

    body.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_a_json_fence() {
        assert_eq!(strip_code_fence("```json\n{\"a\": 1}\n```"), "{\"a\": 1}");
    }

    #[test]
    fn strips_a_bash_fence() {
        assert_eq!(strip_code_fence("```bash\ngit status\n```"), "git status");
    }

    #[test]
    fn strips_a_plain_fence() {
        assert_eq!(strip_code_fence("```\nls -la\n```"), "ls -la");
    }

    #[test]
    fn strips_surrounding_backticks() {
        assert_eq!(strip_code_fence("`ls`"), "ls");
    }

    #[test]
    fn leaves_unfenced_text_alone() {
        assert_eq!(strip_code_fence("  {\"a\": 1}  "), "{\"a\": 1}");
    }

    #[test]
    fn keeps_inner_fences_of_a_multi_block_answer() {
        // Only the outermost fence is removed.
        let text = "```json\n{\"a\": \"```\"}\n```";
        assert_eq!(strip_code_fence(text), "{\"a\": \"```\"}");
    }

    #[test]
    fn drops_an_info_string_with_attributes() {
        assert_eq!(
            strip_code_fence("```sh title=fix\nrm -rf build\n```"),
            "rm -rf build"
        );
    }

    #[test]
    fn a_fence_with_no_body_is_empty() {
        assert_eq!(strip_code_fence("````"), "");
        assert_eq!(strip_code_fence("``"), "");
        assert_eq!(strip_code_fence(""), "");
    }

    #[test]
    fn the_result_is_trimmed() {
        // Callers feed this straight into serde_json or the input buffer.
        assert_eq!(strip_code_fence("```bash\nls -l\n```"), "ls -l");
    }

    #[test]
    fn tolerates_a_missing_closing_fence() {
        assert_eq!(strip_code_fence("```json\n{\"a\": 1}"), "{\"a\": 1}");
    }
}
