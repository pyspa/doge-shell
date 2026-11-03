//! Alias normalization for completion. Reuse syntax-time argv0 rewriting,
//! then parse its effective source so fixed arguments keep their positions.
//! Editor replacement and external-provider input remain the original request.
use super::*;
use std::hash::{Hash, Hasher};

pub(super) fn alias_signature(aliases: &HashMap<String, String>) -> u64 {
    let mut entries: Vec<_> = aliases.iter().collect();
    entries.sort_unstable_by_key(|(name, _)| *name);
    let mut state = std::collections::hash_map::DefaultHasher::new();
    entries.hash(&mut state);
    state.finish()
}

impl IntegratedCompletionEngine {
    pub(super) fn expand_completion_alias(&self, parsed: &mut ParsedCommandLine) {
        let Some(source) = &parsed.source else {
            return;
        };
        // While editing argv0, complete the alias name itself. In particular,
        // a suffix to the right of the cursor must not trigger expansion.
        if source.suppressed || source.cursor_word == 0 {
            return;
        }
        let Some(word) = source.words.first() else {
            return;
        };
        // Rewrite only the selected argv0. The argument being edited may have
        // unfinished quoting, and nested substitutions must stay opaque here.
        // The parser owns bare-word eligibility and one-pass alias semantics.
        let Ok(replacement) =
            crate::parser::rewrite_aliases(&word.raw, Arc::clone(&self.environment))
        else {
            return;
        };
        if replacement.as_ref() == word.raw {
            return;
        }
        let mut input = source.input.clone();
        input.replace_range(word.byte_start..word.byte_end, replacement.as_ref());
        let cursor = source.cursor - (word.byte_end - word.byte_start) + replacement.len();
        let mut expanded = self.parser.parse(&input, cursor);
        expanded.cursor_index = parsed.cursor_index;
        if let Some(expanded_source) = &mut expanded.source {
            expanded_source.multiple |= source.multiple;
        }
        *parsed = expanded;
    }
}
