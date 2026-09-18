//! Tool Search v2: deterministic lexical discovery over MCP tools.
//!
//! A turn never carries every MCP schema: it carries `tool_search`
//! and earns the rest. This module is the discovery half of that trade -
//! candidate collection (from [`McpManager::searchable_tools`], never a
//! second registry), field-weighted ranking, and Top-N selection. Loading
//! is the caller's job: [`McpManager::tool_definitions_for`] resolves the
//! returned names back to schemas, and `turn_support` merges those into the
//! turn's tool list so the next iteration can call them.
//!
//! Local and deterministic throughout: no network, no embeddings, no
//! `HashMap` iteration order in the output. The per-query tokenized fields
//! are the whole "index" - rebuilding them per search is microseconds at
//! MCP scale, so nothing is cached and nothing can go stale.

use crate::chatgpt::mcp::{McpManager, SearchableTool};
use serde_json::{Value, json};

/// Default Top-N: enough to offer a choice, small enough to keep the
/// tool result a pointer rather than a catalogue.
pub(crate) const DEFAULT_LIMIT: usize = 5;
/// Ceiling on an explicit `limit`, so one call cannot reintroduce the
/// dump-every-schema behaviour lazy loading exists to avoid.
pub(crate) const MAX_LIMIT: usize = 20;
/// Cap on query tokens: scoring is linear in their count, and past this
/// point extra words are prose, not search terms.
const MAX_QUERY_TOKENS: usize = 16;
/// Tokens shorter than this never match: without the floor, the `a` in
/// every description matches any query containing the letter by substring.
const MIN_TOKEN_LEN: usize = 2;
/// High-frequency filler words that carry no search signal (`the file`
/// means `file`). Dropped from queries and fields alike, so both sides
/// stay symmetric: a tool named `for_each` is still found by `for each`.
const STOP_WORDS: &[&str] = &[
    "an", "the", "and", "or", "to", "of", "in", "on", "for", "with", "by", "from", "at", "as",
    "is", "are", "be",
];
/// Compact results stay compact: descriptions longer than this are cut at
/// a character boundary, the full text arrives with the loaded schema.
const MAX_DESCRIPTION_CHARS: usize = 240;

/// Field weights: identity first. A tool whose *name* matches is the tool;
/// a tool whose description merely mentions the word might only be near it.
const NAME_WEIGHT: f32 = 5.0;
const GROUP_WEIGHT: f32 = 4.0;
const SERVER_WEIGHT: f32 = 4.0;
const DESCRIPTION_WEIGHT: f32 = 3.0;
const PARAM_NAME_WEIGHT: f32 = 2.0;
const PARAM_DESCRIPTION_WEIGHT: f32 = 1.0;

/// Within one field, a direct hit beats a partial one.
const EXACT_SCORE: f32 = 1.0;
const PREFIX_SCORE: f32 = 0.7;
const SUBSTRING_SCORE: f32 = 0.4;

/// One ranked hit: what the model reads, plus nothing it does not need.
/// The full schema is deliberately absent - the caller loads it through
/// [`McpManager::tool_definitions_for`] only for the tools the model picks.
#[derive(Debug, Clone)]
pub(crate) struct RankedTool {
    pub name: String,
    pub description: String,
    pub server: String,
    pub group: String,
    /// Whether the group toggle already exposes this tool. `false` means
    /// discoverable-but-hidden, not absent: the next iteration can still
    /// call it once loaded, or the whole group via `mcp_load_group`.
    pub active: bool,
    pub score: f32,
}

/// Lowercase, split, de-duplicate: `github_search_issues` becomes
/// `[github, search, issue]`, and so does `"find open GitHub ISSUES"`.
/// Splits on `_ - . / :` and whitespace, folds a trailing `s` plural
/// (`issues` to `issue`) - a normalisation, not stemming - and drops
/// empties, repeats, single characters, and stop words, so a padded query
/// cannot inflate its own score and a one-letter word like the `a` in
/// every description cannot match every query by substring.
///
/// A camelCase chunk contributes both its whole and its parts:
/// `issueNumber` becomes `[issuenumber, issue, number]`, so a query for
/// `issue number` hits exactly, while `GitHub` keeps `github` whole and
/// `github` queries keep exact-matching it.
pub(crate) fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut push = |token: String| {
        if token.len() >= MIN_TOKEN_LEN
            && !STOP_WORDS.contains(&token.as_str())
            && !tokens.contains(&token)
        {
            tokens.push(token);
        }
    };
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        push(fold_plural(&raw.to_lowercase()));
        for part in split_camel(raw) {
            push(fold_plural(&part.to_lowercase()));
        }
    }
    if tokens.len() > MAX_QUERY_TOKENS {
        tokens.truncate(MAX_QUERY_TOKENS);
    }
    tokens
}

/// Lower-to-upper boundary splits (`issueNumber` to `[issue, Number]`),
/// without touching all-caps runs (`HTTPSConnection` stays whole: there
/// is no lowercase-to-upper boundary inside it). The caller lowercases.
fn split_camel(chunk: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut prev_lower = false;
    for ch in chunk.chars() {
        if ch.is_uppercase() && prev_lower {
            parts.push(std::mem::take(&mut current));
        }
        current.push(ch);
        prev_lower = ch.is_lowercase();
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.len() > 1 { parts } else { Vec::new() }
}

/// Minimal plural folding so `issues` matches `issue` exactly rather than
/// merely by prefix: a trailing `ies` becomes `y` (`repositories` to
/// `repository`, `ties` to `ty`), else a trailing `s` is stripped past three
/// characters and never from `ss`. A normalisation, not stemming - just
/// enough for the plurals tool and parameter names actually use.
fn fold_plural(token: &str) -> String {
    if token.len() >= 4 && token.ends_with("ies") {
        return format!("{}y", &token[..token.len() - 3]);
    }
    if token.len() > 3 && token.ends_with('s') && !token.ends_with("ss") {
        token[..token.len() - 1].to_string()
    } else {
        token.to_string()
    }
}

/// Exact beats prefix beats substring. Only the field containing the
/// query counts as a partial hit - never the reverse: a query merely
/// containing a short field token (`train` containing `in`) is not
/// evidence the tool is relevant.
fn match_level(query: &str, field: &str) -> f32 {
    if query == field {
        EXACT_SCORE
    } else if field.starts_with(query) {
        PREFIX_SCORE
    } else if field.contains(query) {
        SUBSTRING_SCORE
    } else {
        0.0
    }
}

/// Mean best-match over the query tokens, so a field saturates at its
/// weight no matter how often one word repeats in it: a description of
/// "issue issue issue" scores no higher than one that says it once.
fn field_score(query_tokens: &[String], field_tokens: &[String]) -> f32 {
    if query_tokens.is_empty() || field_tokens.is_empty() {
        return 0.0;
    }
    let sum: f32 = query_tokens
        .iter()
        .map(|query| {
            field_tokens
                .iter()
                .map(|field| match_level(query, field))
                .fold(0.0, f32::max)
        })
        .sum();
    sum / query_tokens.len() as f32
}

/// Tokenized search fields for one candidate: the transient per-search
/// index. Built once per search from [`SearchableTool`], dropped after.
struct FieldTokens {
    name: Vec<String>,
    group: Vec<String>,
    server: Vec<String>,
    description: Vec<String>,
    param_names: Vec<String>,
    param_descriptions: Vec<String>,
}

impl FieldTokens {
    fn for_tool(tool: &SearchableTool) -> Self {
        let params = |texts: &[String]| {
            texts
                .iter()
                .flat_map(|text| tokenize(text))
                .collect::<Vec<_>>()
        };
        Self {
            name: tokenize(&tool.function_name),
            group: tokenize(&tool.group),
            server: tokenize(&tool.server_label),
            description: tokenize(&tool.description),
            param_names: params(&tool.param_names),
            param_descriptions: params(&tool.param_descriptions),
        }
    }

    fn score(&self, query_tokens: &[String]) -> f32 {
        NAME_WEIGHT * field_score(query_tokens, &self.name)
            + GROUP_WEIGHT * field_score(query_tokens, &self.group)
            + SERVER_WEIGHT * field_score(query_tokens, &self.server)
            + DESCRIPTION_WEIGHT * field_score(query_tokens, &self.description)
            + PARAM_NAME_WEIGHT * field_score(query_tokens, &self.param_names)
            + PARAM_DESCRIPTION_WEIGHT * field_score(query_tokens, &self.param_descriptions)
    }
}

/// Rank the manager's discoverable tools for `query`, best first, at most
/// `limit`. Ties break on tool name ascending - never on map order - so
/// repeated searches and test runs see the same list.
///
/// Test-only: production goes through [`run`], which builds the candidate
/// list once and shares it with the result rendering instead of walking
/// the bindings twice.
#[cfg(test)]
pub(crate) fn search(manager: &McpManager, query: &str, limit: usize) -> Vec<RankedTool> {
    search_candidates(&manager.searchable_tools(), query, limit)
}

fn search_candidates(candidates: &[SearchableTool], query: &str, limit: usize) -> Vec<RankedTool> {
    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        return Vec::new();
    }
    let limit = limit.clamp(1, MAX_LIMIT);
    let mut hits: Vec<RankedTool> = candidates
        .iter()
        .filter_map(|tool| {
            let score = FieldTokens::for_tool(tool).score(&query_tokens);
            if score > 0.0 {
                Some(RankedTool {
                    name: tool.function_name.clone(),
                    description: truncate_description(&tool.description),
                    server: tool.server_label.clone(),
                    group: tool.group.clone(),
                    active: tool.group_enabled,
                    score,
                })
            } else {
                None
            }
        })
        .collect();
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.name.cmp(&right.name))
    });
    hits.truncate(limit);
    hits
}

fn truncate_description(description: &str) -> String {
    if description.len() <= MAX_DESCRIPTION_CHARS {
        return description.to_string();
    }
    let end = description.floor_char_boundary(MAX_DESCRIPTION_CHARS);
    format!("{}...", description[..end].trim_end())
}

/// Schema definition for the `tool_search` tool, shared by agent and
/// interactive turns. Discovery only: calling it never executes an MCP tool.
pub(crate) fn definition() -> Value {
    crate::agent::definition(
        "tool_search",
        "Find MCP tools by words in their name, description, parameters, or server/group. Returns compact ranked matches with server, group, and whether each tool is already active. Discovered tools become callable on the next model request; use mcp_load_group only to activate a whole group at once. Discovery does not authorize execution.",
        serde_json::json!({"query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":20,"description":"Maximum matches to return. Defaults to 5."}}),
        &["query"],
    )
}

/// Run the `tool_search` tool: parse `{query, limit?}`, rank, and render
/// the compact result the model reads. Never executes an MCP tool.
pub(crate) fn run(manager: &McpManager, arguments: &str) -> Result<String, String> {
    let args: Value = serde_json::from_str(arguments).map_err(|err| err.to_string())?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or("query required")?;
    if query.trim().is_empty() {
        return Err("nonempty query required".into());
    }
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|limit| (limit.max(1) as usize).min(MAX_LIMIT))
        .unwrap_or(DEFAULT_LIMIT);

    let candidates = manager.searchable_tools();
    let hits = search_candidates(&candidates, query, limit);
    tracing::debug!(
        query,
        candidates = candidates.len(),
        results = hits.len(),
        top = hits.first().map(|hit| hit.score).unwrap_or(0.0),
        "tool_search ranked"
    );
    if hits.is_empty() {
        return Ok(json!({
            "query": query,
            "count": 0,
            "results": [],
            "message": format!("No matching MCP tools found for query: {query}"),
        })
        .to_string());
    }
    let results: Vec<Value> = hits
        .iter()
        .map(|hit| {
            json!({
                "name": hit.name,
                "description": hit.description,
                "server": hit.server,
                "group": hit.group,
                "score": (hit.score * 10.0).round() / 10.0,
                "active": hit.active,
            })
        })
        .collect();
    Ok(json!({
        "query": query,
        "count": results.len(),
        "results": results,
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_splits_separators_and_case() {
        assert_eq!(
            tokenize("github_search_issues"),
            vec!["github", "search", "issue"]
        );
        assert_eq!(
            tokenize("find open GitHub ISSUES"),
            vec!["find", "open", "github", "git", "hub", "issue"]
        );
        assert_eq!(
            tokenize("mcp__github__get-issue.number:count"),
            vec!["mcp", "github", "get", "issue", "number", "count"]
        );
    }

    #[test]
    fn tokenize_dedupes_and_drops_single_characters() {
        assert_eq!(tokenize("issue issue  ISSUES"), vec!["issue"]);
        // Without the length floor the `a` in every description would
        // substring-match any query containing that letter.
        assert_eq!(tokenize("a b read"), vec!["read"]);
    }

    #[test]
    fn tokenize_folds_ies_plurals_and_caps_token_count() {
        assert_eq!(tokenize("ties"), vec!["ty"]);
        assert_eq!(tokenize("repositories"), vec!["repository"]);
        let many = (0..30)
            .map(|index| format!("word{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(tokenize(&many).len(), MAX_QUERY_TOKENS);
    }

    #[test]
    fn tokenize_drops_stop_words_from_queries_and_fields_alike() {
        assert_eq!(tokenize("read the file"), vec!["read", "file"]);
        assert_eq!(tokenize("the"), Vec::<String>::new());
    }

    #[test]
    fn tokenize_keeps_camel_whole_and_parts() {
        // The whole keeps `GitHub` exact-matching `github`; the parts let
        // `issue number` reach a camelCase parameter name.
        assert_eq!(
            tokenize("issueNumber"),
            vec!["issuenumber", "issue", "number"]
        );
        // All-caps runs have no lower-to-upper boundary and stay whole.
        assert_eq!(tokenize("HTTPSConnection"), vec!["httpsconnection"]);
    }

    #[test]
    fn match_levels_prefer_exact_over_prefix_over_substring() {
        assert!(match_level("issue", "issue") > match_level("iss", "issue"));
        assert!(match_level("iss", "issue") > match_level("ssu", "issue"));
        assert_eq!(match_level("slack", "issue"), 0.0);
        // A query containing a short field token is not a partial hit.
        assert_eq!(match_level("training", "in"), 0.0);
    }

    #[test]
    fn repeated_query_terms_do_not_inflate_a_field() {
        let once = field_score(&["issue".to_string()], &["issue".to_string()]);
        let padded = field_score(
            &["issue".to_string(), "issue".to_string()],
            &["issue".to_string()],
        );
        assert_eq!(once, padded);
    }

    #[test]
    fn long_descriptions_truncate_at_a_character_boundary() {
        assert_eq!(truncate_description("short"), "short");
        // A byte-length cut would panic or split a character on a
        // multibyte prefix; `floor_char_boundary` keeps this intact.
        let long = format!("説明{}", "x".repeat(MAX_DESCRIPTION_CHARS));
        let truncated = truncate_description(&long);
        assert!(truncated.starts_with("説明"));
        assert!(truncated.ends_with("..."));
        assert!(truncated.len() < long.len() + "...".len());
    }

    /// A hand-rolled manager pair: equal scores must still order by name.
    #[test]
    fn ties_break_on_tool_name() {
        let mut manager = McpManager::default();
        manager.insert_test_tool("srv", "zzz");
        manager.insert_test_tool("srv", "aaa");

        let hits = search(&manager, "srv", 5);
        let names: Vec<&str> = hits.iter().map(|hit| hit.name.as_str()).collect();
        assert_eq!(names, vec!["mcp__srv__aaa", "mcp__srv__zzz"]);
    }

    #[test]
    fn search_is_case_insensitive() {
        let mut manager = McpManager::default();
        manager.insert_test_tool("github", "search_issues");

        for query in ["GitHub", "github", "GITHUB"] {
            let hits = search(&manager, query, 5);
            assert_eq!(hits.len(), 1, "query {query}");
            assert_eq!(hits[0].name, "mcp__github__search_issues");
        }
    }
}
