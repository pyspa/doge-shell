//! Locate argv0 without discarding source words. Prefix syntax is recognized
//! literally, following assign_name/assignment and redirect in shell.pest; no
//! value, expansion or substitution is evaluated by completion.
use super::*;

pub(super) fn command_word(source: &str, spans: &[ShellTokenSpan]) -> (usize, bool) {
    let mut index = 0;
    while let Some(word) = spans.get(index) {
        // simple_command requires whitespace between prefix elements and argv0.
        if index > 0 && word.byte_start == spans[index - 1].byte_end && !word.raw.is_empty() {
            return (index, true);
        }
        if CommandLineParser::is_redirect_operator(&word.raw) {
            if let Some(target) = spans.get(index + 1)
                && (CommandLineParser::is_redirect_operator(&target.raw)
                    || (CommandLineParser::is_descriptor_redirect(&word.raw)
                        && !target.raw.is_empty()
                        && !valid_descriptor_target(source, word, target)))
            {
                return (index, true);
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

fn valid_descriptor_target(
    source: &str,
    operator: &ShellTokenSpan,
    target: &ShellTokenSpan,
) -> bool {
    let gap = &source[operator.byte_end..target.byte_start];
    let value = crate::parser::check::join_continued_lines(&target.raw);
    gap.trim_start_matches("\\\n").is_empty()
        && (value == "-" || value.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_assignment_word(raw: &str) -> bool {
    raw.split_once('=').is_some_and(|(name, _)| {
        let name = crate::parser::check::join_continued_lines(name);
        !name.is_empty()
            && name.chars().enumerate().all(|(index, ch)| {
                ch == '_' || ch.is_ascii_alphabetic() || (index > 0 && ch.is_ascii_digit())
            })
    })
}
