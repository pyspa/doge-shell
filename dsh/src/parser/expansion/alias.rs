//! Syntax-time alias rewriting: replace static `argv0` spans only.
//!
//! Alias changes command syntax itself, so it runs on the source text before
//! strict parsing. Variable, tilde, brace/glob and substitution expansion are
//! runtime concerns and never happen here.
use crate::parser::{Rule, ShellParser};
use anyhow::Result;
use parking_lot::RwLock;
use pest::Parser as _;
use pest::iterators::Pair;
use std::collections::HashMap;
use std::sync::Arc;

use crate::environment::Environment;

struct AliasEdit {
    start: usize,
    end: usize,
    replacement: String,
}

/// Rewrite static `argv0` words that name an alias, leaving everything else
/// byte-for-byte intact.
///
/// Only a bare `word` names an alias. A `$CMD`, quoted word, or substitution
/// result is runtime data and is left for materialization.
pub fn rewrite_aliases<'a>(
    input: &'a str,
    environment: Arc<RwLock<Environment>>,
) -> Result<std::borrow::Cow<'a, str>> {
    let aliases: HashMap<String, String> = environment.read().variable_state.alias.clone();
    if aliases.is_empty() {
        return Ok(std::borrow::Cow::Borrowed(input));
    }
    let pairs = match ShellParser::parse(Rule::commands, input) {
        Ok(pairs) => pairs,
        Err(_) => return Ok(std::borrow::Cow::Borrowed(input)),
    };
    let mut edits = Vec::new();
    for pair in pairs {
        collect_alias_edits(pair, &aliases, &mut edits);
    }
    if edits.is_empty() {
        return Ok(std::borrow::Cow::Borrowed(input));
    }
    edits.sort_by_key(|a| std::cmp::Reverse(a.start));
    let mut out = input.to_string();
    for edit in edits {
        out.replace_range(edit.start..edit.end, &edit.replacement);
    }
    Ok(std::borrow::Cow::Owned(out))
}

fn static_argv0_name(pair: &Pair<Rule>) -> Option<(usize, usize, String)> {
    let mut spans = pair.clone().into_inner();
    let span = spans.next()?;
    if spans.next().is_some() {
        return None;
    }
    let mut parts = span.clone().into_inner();
    let part = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if !matches!(part.as_rule(), Rule::word) {
        return None;
    }
    let name = part.as_str().to_string();
    let span_pos = span.as_span();
    Some((span_pos.start(), span_pos.end(), name))
}

fn collect_alias_edits(
    pair: Pair<Rule>,
    aliases: &HashMap<String, String>,
    edits: &mut Vec<AliasEdit>,
) {
    if pair.as_rule() == Rule::argv0
        && let Some((start, end, name)) = static_argv0_name(&pair)
        && let Some(replacement) = aliases.get(&name)
    {
        edits.push(AliasEdit {
            start,
            end,
            replacement: replacement.clone(),
        });
    }
    for inner in pair.into_inner() {
        collect_alias_edits(inner, aliases, edits);
    }
}
