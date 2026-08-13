use std::collections::HashMap;

pub(crate) fn resolve(
    input: &str,
    cursor_chars: usize,
    word: &str,
    global: &HashMap<String, String>,
    scoped: &HashMap<String, HashMap<String, String>>,
) -> Option<String> {
    if word.contains(['\'', '"']) {
        return None;
    }
    let cursor_byte = input
        .char_indices()
        .nth(cursor_chars)
        .map_or(input.len(), |(offset, _)| offset);
    let prefix = &input[..cursor_byte];
    let segment = current_segment(prefix)?;
    let command = direct_command(segment)?;

    scoped
        .get(command)
        .and_then(|entries| entries.get(word))
        .cloned()
        .or_else(|| global.get(word).cloned())
}

fn current_segment(input: &str) -> Option<&str> {
    let mut quote = None;
    let mut escaped = false;
    let mut start = 0;
    for (offset, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some(current) => {
                if current == '"' && ch == '\\' {
                    escaped = true;
                } else if ch == current {
                    quote = None;
                }
            }
            None => match ch {
                '\\' => escaped = true,
                '\'' | '"' => quote = Some(ch),
                '|' | ';' | '&' => start = offset + ch.len_utf8(),
                _ => {}
            },
        }
    }
    quote.is_none().then_some(&input[start..])
}

fn direct_command(segment: &str) -> Option<&str> {
    let segment = segment.trim_start();
    let end = segment
        .find(|ch: char| ch.is_whitespace())
        .unwrap_or(segment.len());
    (end > 0).then_some(&segment[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maps() -> (
        HashMap<String, String>,
        HashMap<String, HashMap<String, String>>,
    ) {
        let global = HashMap::from([("co".to_string(), "global-co".to_string())]);
        let scoped = HashMap::from([(
            "git".to_string(),
            HashMap::from([("co".to_string(), "checkout".to_string())]),
        )]);
        (global, scoped)
    }

    #[test]
    fn scoped_definition_wins_over_global() {
        let (global, scoped) = maps();
        assert_eq!(
            resolve("git co", 6, "co", &global, &scoped).as_deref(),
            Some("checkout")
        );
        assert_eq!(
            resolve("hg co", 5, "co", &global, &scoped).as_deref(),
            Some("global-co")
        );
    }

    #[test]
    fn pipeline_uses_its_current_direct_command() {
        let (global, scoped) = maps();
        assert_eq!(
            resolve("echo x | git co", 15, "co", &global, &scoped).as_deref(),
            Some("checkout")
        );
    }

    #[test]
    fn quoted_words_do_not_expand() {
        let (global, scoped) = maps();
        assert_eq!(resolve("git 'co'", 8, "'co'", &global, &scoped), None);
        assert_eq!(resolve("echo \"x co", 10, "co", &global, &scoped), None);
        assert_eq!(resolve("echo 'x co", 10, "co", &global, &scoped), None);
    }
}
