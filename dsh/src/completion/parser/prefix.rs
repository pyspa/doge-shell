//! Locate argv0 without discarding source words. Prefix syntax is recognized
//! literally, following assign_name/assignment and redirect in shell.pest; no
//! value, expansion or substitution is evaluated by completion.
use super::*;

pub(super) fn command_word(spans: &[ShellTokenSpan]) -> (usize, bool) {
    let mut index = 0;
    while let Some(word) = spans.get(index) {
        // simple_command requires whitespace between prefix elements and argv0.
        if index > 0 && word.byte_start == spans[index - 1].byte_end && !word.raw.is_empty() {
            return (index, true);
        }
        if CommandLineParser::is_redirect_operator(&word.raw) {
            if let Some(target) = spans.get(index + 1) {
                let descriptor = word.raw.ends_with(">&") || word.raw.ends_with("<&");
                if CommandLineParser::is_redirect_operator(&target.raw)
                    || (descriptor
                        && !target.raw.is_empty()
                        && (target.byte_start != word.byte_end
                            || (target.raw != "-"
                                && !target.raw.chars().all(|ch| ch.is_ascii_digit()))))
                {
                    return (index, true);
                }
            }
            index += 2;
        } else if is_assignment_word(&word.raw) {
            index += 1;
        } else {
            break;
        }
    }
    (index.min(spans.len()), false)
}

fn is_assignment_word(raw: &str) -> bool {
    raw.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().enumerate().all(|(index, ch)| {
                ch == '_' || ch.is_ascii_alphabetic() || (index > 0 && ch.is_ascii_digit())
            })
    })
}
