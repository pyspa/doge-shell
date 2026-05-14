#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AiWatchRequest {
    pub goal: Option<String>,
    pub command: String,
}

pub(crate) fn parse_ai_watch(input: &str) -> Result<Option<AiWatchRequest>, String> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with("ai-watch") {
        return Ok(None);
    }

    let after_name = &trimmed["ai-watch".len()..];
    if !after_name.is_empty()
        && !after_name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_whitespace())
    {
        return Ok(None);
    }

    let Some((delimiter_start, delimiter_end)) = find_double_dash_token(trimmed) else {
        return Err("ai-watch requires `-- <command>`".to_string());
    };

    let prefix = trimmed[..delimiter_start].trim();
    let command = trimmed[delimiter_end..].trim();
    if command.is_empty() {
        return Err("ai-watch requires a command after `--`".to_string());
    }

    let words =
        shell_words::split(prefix).map_err(|err| format!("invalid ai-watch options: {err}"))?;
    let mut goal = None;
    let mut index = 1;
    while index < words.len() {
        match words[index].as_str() {
            "--goal" | "-g" => {
                index += 1;
                let Some(value) = words.get(index) else {
                    return Err("--goal requires a value".to_string());
                };
                goal = Some(value.clone());
            }
            value if value.starts_with("--goal=") => {
                goal = Some(value.trim_start_matches("--goal=").to_string());
            }
            value => return Err(format!("unknown ai-watch option: {value}")),
        }
        index += 1;
    }

    Ok(Some(AiWatchRequest {
        goal,
        command: command.to_string(),
    }))
}

pub(crate) fn wrap_current_input(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.starts_with("ai-watch ") || trimmed == "ai-watch" {
        return None;
    }
    Some(format!("ai-watch -- {trimmed}"))
}

fn find_double_dash_token(input: &str) -> Option<(usize, usize)> {
    let mut quote = None;
    let mut escaped = false;
    let mut token_start = None;

    for (idx, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            continue;
        }

        if ch.is_ascii_whitespace() {
            token_start = None;
            continue;
        }

        let start = token_start.get_or_insert(idx);
        if *start == idx
            && input[idx..].starts_with("--")
            && input[idx + 2..]
                .chars()
                .next()
                .is_none_or(|next| next.is_ascii_whitespace())
        {
            return Some((idx, idx + 2));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_requires_ai_watch_prefix() {
        assert_eq!(parse_ai_watch("echo hi").unwrap(), None);
    }

    #[test]
    fn parse_plain_command() {
        let parsed = parse_ai_watch("ai-watch -- cargo test -p doge-shell")
            .unwrap()
            .unwrap();
        assert_eq!(parsed.goal, None);
        assert_eq!(parsed.command, "cargo test -p doge-shell");
    }

    #[test]
    fn parse_goal_with_spaces() {
        let parsed = parse_ai_watch("ai-watch --goal 'find failures' -- cargo test")
            .unwrap()
            .unwrap();
        assert_eq!(parsed.goal.as_deref(), Some("find failures"));
        assert_eq!(parsed.command, "cargo test");
    }

    #[test]
    fn parse_preserves_shell_command_after_delimiter() {
        let parsed = parse_ai_watch("ai-watch -g ready -- npm run dev | tee out.log")
            .unwrap()
            .unwrap();
        assert_eq!(parsed.goal.as_deref(), Some("ready"));
        assert_eq!(parsed.command, "npm run dev | tee out.log");
    }

    #[test]
    fn wrap_current_input_uses_explicit_delimiter() {
        assert_eq!(
            wrap_current_input("cargo test"),
            Some("ai-watch -- cargo test".to_string())
        );
        assert_eq!(wrap_current_input(""), None);
        assert_eq!(wrap_current_input("ai-watch -- echo hi"), None);
    }
}
