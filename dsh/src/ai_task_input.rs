//! Recognize the interactive background AI prefix before shell syntax.

#[derive(PartialEq, Eq)]
pub(crate) enum TaskInput<'a> {
    Menu,
    Start { edit: bool, prompt: &'a str },
}

pub(crate) fn classify(input: &str) -> Option<Result<TaskInput<'_>, &'static str>> {
    let rest = input.trim_start().strip_prefix("!&")?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
