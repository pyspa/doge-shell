//! Recognize AI prompts and shortcuts in the shell input buffer.

use crate::completion::command_segment::{PipeTrigger, top_level_pipe_marker};
use crate::parser::check::{SHELL_WHITESPACE, after_leading_continuations};

#[derive(PartialEq, Eq)]
pub(crate) enum TaskInput<'a> {
    Menu,
    Start { edit: bool, prompt: &'a str },
}

/// Skip shell whitespace and line continuations before the marker. Once the
/// prompt starts, its text belongs to the chat and remains literal.
pub(crate) fn chat_prompt(input: &str) -> Option<&str> {
    after_leading_continuations(input)
        .trim_start_matches(SHELL_WHITESPACE)
        .strip_prefix('!')
}

pub(crate) fn classify(input: &str) -> Option<Result<TaskInput<'_>, &'static str>> {
    let rest = chat_prompt(input)?.strip_prefix('&')?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return Some(Err("!& needs whitespace before the task text"));
    }
    let mut rest = rest.trim_start();
    if rest.is_empty() {
        return Some(Ok(TaskInput::Menu));
    }
    let mut edit = false;
    loop {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        match &rest[..end] {
            "--edit" if !edit => edit = true,
            "--" => {
                rest = rest[end..].trim_start();
                break;
            }
            option if option.starts_with('-') => {
                return Some(Err("!& accepts only --edit and -- before the task text"));
            }
            _ => break,
        }
        rest = rest[end..].trim_start();
    }
    if rest.is_empty() {
        Some(Err("!& requires task text after its options"))
    } else {
        Some(Ok(TaskInput::Start { edit, prompt: rest }))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TabAction<'a> {
    Shell,
    Literal,
    Generate(&'a str),
    ExpandPipe { marker: usize, query: &'a str },
}

/// Classify the cursor before running any Tab hook, provider or AI request.
pub(crate) fn tab_action(input: &str, cursor: usize) -> TabAction<'_> {
    let head = after_leading_continuations(input).trim_start_matches(SHELL_WHITESPACE);
    if head.starts_with('!') {
        return TabAction::Literal;
    }
    let generative = head.starts_with("??");
    let marker = if generative {
        input.len() - head.len()
    } else if let Some(marker) = top_level_pipe_marker(input, PipeTrigger::Tab) {
        marker
    } else {
        return TabAction::Shell;
    };
    if crate::completion::command_segment::char_to_byte(input, cursor) < marker + 2 {
        return TabAction::Shell;
    }
    let query = input[marker + 2..].trim();
    if query.is_empty() || input[marker..].starts_with("|!") {
        return TabAction::Literal;
    }
    if generative {
        TabAction::Generate(query)
    } else {
        TabAction::ExpandPipe { marker, query }
    }
}

/// Splits `command |! query` at the first top-level `|!`. A `|!` inside
/// quotes or escapes belongs to the command, so `grep 'a|!b' f` is not an AI
/// pipe. Surrounding quotes around the query are removed.
pub(crate) fn output_pipe(input: &str) -> Option<(&str, &str)> {
    let idx = top_level_pipe_marker(input, PipeTrigger::Enter)?;
    let command = input[..idx].trim();
    let query_part = input[idx + 2..].trim();
    let query = query_part
        .strip_prefix('"')
        .and_then(|q| q.strip_suffix('"'))
        .or_else(|| {
            query_part
                .strip_prefix('\'')
                .and_then(|q| q.strip_suffix('\''))
        })
        .unwrap_or(query_part);

    if command.is_empty() || query.is_empty() {
        return None;
    }
    Some((command, query))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_query_belongs_to_the_cursor_side_of_the_marker() {
        let input = "日本語 |? summarize";
        for cursor in [0, 4, 5] {
            assert_eq!(tab_action(input, cursor), TabAction::Shell);
        }
        for cursor in [6, 9, input.chars().count()] {
            assert_eq!(
                tab_action(input, cursor),
                TabAction::ExpandPipe {
                    marker: input.find('|').unwrap(),
                    query: "summarize"
                }
            );
        }
        let input = "  ?? explain |? literally";
        for cursor in [0, 2, 3] {
            assert_eq!(tab_action(input, cursor), TabAction::Shell);
        }
        assert_eq!(
            tab_action(input, 4),
            TabAction::Generate("explain |? literally")
        );
    }

    #[test]
    fn tab_uses_shell_whitespace_and_keeps_prompt_text_literal() {
        for input in [
            "ls |?",
            "ls |?   ",
            "??",
            "echo |! explain |? literally",
            " \\\n!& task |? literally",
        ] {
            assert_eq!(
                tab_action(input, input.chars().count()),
                TabAction::Literal,
                "{input:?}"
            );
        }
        for input in ["ls | grep foo", "\u{a0}?? literal", r"\?? literal"] {
            assert_eq!(
                tab_action(input, input.chars().count()),
                TabAction::Shell,
                "{input:?}"
            );
        }
        let input = " \\\n\t?? show |! and |?";
        assert_eq!(
            tab_action(input, input.chars().count()),
            TabAction::Generate("show |! and |?")
        );
        let input = "cat file | head -10 |? explain |! literally";
        assert_eq!(
            tab_action(input, input.chars().count()),
            TabAction::ExpandPipe {
                marker: input.find("|?").unwrap(),
                query: "explain |! literally"
            }
        );
    }

    #[test]
    fn chat_marker_can_follow_continuations_without_changing_the_prompt() {
        assert_eq!(chat_prompt(" \\\n \\\n!a\\\nb"), Some("a\\\nb"));
        assert!(matches!(
            classify("\\\n!& fix what's here\\\nnow"),
            Some(Ok(TaskInput::Start {
                edit: false,
                prompt: "fix what's here\\\nnow"
            }))
        ));
        assert_eq!(chat_prompt(r"\!escaped"), None);
        assert!(classify("\\\n\u{a0}!& task").is_none());
    }

    #[test]
    fn task_text_is_literal_and_options_are_only_read_at_the_start() {
        let raw = "調査 'broken $HOME $(touch marker) |! query &  \nend";
        for (input, edit, prompt) in [
            (format!("!& {raw}"), false, raw),
            (format!("  !& --edit  {raw}"), true, raw),
            ("!& -- --flag".into(), false, "--flag"),
            ("!& --edit -- --edit".into(), true, "--edit"),
            ("!& body --edit".into(), false, "body --edit"),
        ] {
            assert!(
                matches!(classify(&input), Some(Ok(TaskInput::Start {edit: actual, prompt: body})) if actual == edit && body == prompt)
            );
        }
        for input in ["!&", " \t!&  "] {
            assert!(matches!(classify(input), Some(Ok(TaskInput::Menu))));
        }
        for input in [
            "!&foo",
            "!& --edit",
            "!& --",
            "!& --unknown x",
            "!& --edit --edit x",
            "!& -x",
        ] {
            assert!(matches!(classify(input), Some(Err(_))), "{input}");
        }
        for input in ["! hello", "!!", "echo hi &", "echo !&", "echo a\n!& hello"] {
            assert!(classify(input).is_none(), "{input}");
        }
    }
}
