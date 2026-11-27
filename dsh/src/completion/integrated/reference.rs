//! Locate the shell reference being edited and map its source span back to
//! the original editor cursor, including inside aliases and command wrappers.
use super::*;
use crate::parser::{Rule, ShellParser};
use pest::Parser;
use std::ops::Range;

enum ReferenceKind {
    Variable,
    BracedVariable,
    Home,
}

pub(super) struct Reference<'a> {
    kind: ReferenceKind,
    prefix: Cow<'a, str>,
    span: Range<usize>,
}

fn reference_at(word: &str, cursor: usize) -> Option<Reference<'_>> {
    if let Some(home) = word.strip_prefix('~') {
        let name = home.split('/').next()?;
        let end = 1 + name.len();
        if (1..=end).contains(&cursor) && name.chars().all(valid_user_char) {
            return Some(Reference {
                kind: ReferenceKind::Home,
                prefix: Cow::Borrowed(&word[1..cursor]),
                span: 0..end,
            });
        }
    }
    if !word[..cursor].contains('$') {
        return None;
    }
    let mut parts = ShellParser::parse(Rule::completion_reference_word, word)
        .ok()?
        .next()?
        .into_inner()
        .peekable();
    while let Some(part) = parts.next() {
        let kind = match part.as_rule() {
            Rule::completion_ref_plain => ReferenceKind::Variable,
            Rule::completion_ref_braced => ReferenceKind::BracedVariable,
            _ => continue,
        };
        let span = part.as_span();
        let name = part
            .into_inner()
            .find(|p| p.as_rule() == Rule::completion_ref_name)?
            .as_span();
        // Tab immediately after a closing double quote still edits the
        // enclosed name. The quote itself stays in the editor.
        let after_quote = parts.peek().is_some_and(|p| {
            p.as_rule() == Rule::completion_ref_close_quote
                && p.as_span().start() == name.end()
                && p.as_span().end() == cursor
        });
        let end = if after_quote { name.end() } else { cursor };
        if (name.start()..=name.end()).contains(&end) {
            let raw = &word[name.start()..end];
            let prefix = if raw.contains("\\\n") {
                Cow::Owned(raw.replace("\\\n", ""))
            } else {
                Cow::Borrowed(raw)
            };
            return Some(Reference {
                kind,
                prefix,
                span: span.start()..span.end(),
            });
        }
    }
    None
}

fn valid_user_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')
}

impl<'a> Reference<'a> {
    pub(super) fn at_cursor(
        parsed: &'a ParsedCommandLine,
        editor_cursor: usize,
    ) -> Option<(Self, CompletionReplacementRange)> {
        let source = parsed.source.as_ref()?;
        let word = source.words.get(source.cursor_word)?;
        let cursor = source.cursor.checked_sub(word.byte_start)?;
        let before_cursor = word.raw.get(..cursor)?;
        let reference = reference_at(&word.raw, cursor)?;
        let word_start = editor_cursor.checked_sub(before_cursor.chars().count())?;
        let start = word_start + word.raw[..reference.span.start].chars().count();
        let end = start + word.raw[reference.span.clone()].chars().count();
        Some((reference, CompletionReplacementRange { start, end }))
    }

    pub(super) fn candidates(
        &self,
        engine: &IntegratedCompletionEngine,
    ) -> Option<Vec<EnhancedCandidate>> {
        Some(match self.kind {
            ReferenceKind::Variable => {
                engine.variable_candidates(&self.prefix, |name| format!("${name}"))
            }
            ReferenceKind::BracedVariable => {
                engine.variable_candidates(&self.prefix, |name| format!("${{{name}}}"))
            }
            ReferenceKind::Home => {
                let users = tilde_user_candidates(&self.prefix);
                // An unknown ~user can still be a literal file in the cwd.
                if users.is_empty() {
                    return None;
                }
                users
            }
        })
    }

    pub(super) fn ghost_suffix(
        &self,
        engine: &IntegratedCompletionEngine,
        typed: &str,
    ) -> Option<String> {
        if matches!(self.kind, ReferenceKind::Home) {
            return tilde_user_candidates(&self.prefix)
                .into_iter()
                .find_map(|candidate| {
                    let suffix = candidate.text.strip_prefix(typed)?;
                    (!suffix.is_empty()).then(|| suffix.to_owned())
                });
        }
        let braced = matches!(self.kind, ReferenceKind::BracedVariable);
        let env = engine.environment.read();
        let name = matching_variable_names(&env, &self.prefix)
            .filter(|name| braced || name.len() > self.prefix.len())
            .min()?;
        let tail = &name[self.prefix.len()..];
        Some(if braced {
            format!("{tail}}}")
        } else {
            tail.to_owned()
        })
    }
}

pub(super) fn matching_variable_names<'a>(
    environment: &'a Environment,
    prefix: &'a str,
) -> impl Iterator<Item = &'a str> {
    environment
        .variable_state
        .variables
        .keys()
        .map(String::as_str)
        .filter(move |name| {
            name.starts_with(prefix) && dsh_types::shell_variable::is_valid_shell_var_name(name)
        })
}

/// User-home (`~user`) candidates, reusing the `/etc/passwd`-backed user
/// generator. `prefix` is the partial user name (the part after `~`); the
/// returned text keeps the leading `~` so it replaces the whole token.
fn tilde_user_candidates(prefix: &str) -> Vec<EnhancedCandidate> {
    let generator = crate::completion::generators::user::UserGenerator::new();
    match generator.generate_candidates(prefix) {
        Ok(users) => users
            .into_iter()
            .filter(|user| !user.text.is_empty() && user.text.chars().all(valid_user_char))
            .map(|user| EnhancedCandidate {
                text: format!("~{}", user.text),
                description: user.description,
                candidate_type: CandidateType::HomeReference,
                priority: 140,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_token_dispatch_ignores_normal_tokens() {
        for word in ["git", "--flag", "$HOME/foo"] {
            assert!(reference_at(word, word.len()).is_none(), "{word}");
        }
        assert!(reference_at("$", 1).is_some());
    }
}
