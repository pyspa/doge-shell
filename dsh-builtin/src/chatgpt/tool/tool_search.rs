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
use std::collections::{BTreeSet, HashMap};

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
        "Find MCP tools by words in their name, description, parameters, or server/group. Returns compact ranked matches with server, group, and whether each tool is already active. Matching tools can become available on the next model request, subject to the per-turn Tool Search exposure budget; use mcp_load_group only to activate a whole group at once. Discovery does not authorize execution.",
        serde_json::json!({"query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":20,"description":"Maximum matches to return. Defaults to 5."}}),
        &["query"],
    )
}

/// Per-turn cap on schemas `tool_search` may newly expose: without it,
/// repeating searches accumulates every MCP schema and defeats lazy loading.
pub(crate) const MAX_TOOL_SEARCH_EXPOSED_TOOLS_PER_TURN: usize = 32;
/// Byte twin of the count cap: one giant schema must not eat the whole
/// prompt either. Measured in compact-JSON serialized UTF-8 bytes, not
/// tokens, so no tokenizer is needed. Sized from the eval catalog (p50
/// ~0.3 KiB, max ~0.5 KiB per tool): 32 max-size tools cost ~14 KiB, so
/// the count budget binds first for normal tools and this only trips on
/// genuinely oversized single schemas.
pub(crate) const MAX_TOOL_SEARCH_SCHEMA_BYTES_PER_TURN: usize = 96 * 1024;

/// Ephemeral per-turn Tool Search state, shared by interactive and agent
/// turns: which tools this turn's searches already charged, and how many
/// serialized schema bytes they cost. Names only, never schemas - the
/// caller re-resolves through [`McpManager::tool_definitions_for`] on every
/// request, so a disconnect or refresh that removes a tool drops it instead
/// of serving a stale copy. One value lives for one user turn; the next
/// turn starts from [`Default::default`]. Monotonic within the turn: a
/// charged tool that later disappears keeps its budget consumed, so
/// connect/disconnect cycles cannot launder extra exposure.
#[derive(Debug, Default)]
pub(crate) struct ToolSearchExposure {
    loaded_tool_names: BTreeSet<String>,
    used_schema_bytes: usize,
}

impl ToolSearchExposure {
    pub(crate) fn names(&self) -> impl Iterator<Item = &String> {
        self.loaded_tool_names.iter()
    }

    /// Tools charged through Tool Search so far this turn.
    pub(crate) fn charged_tool_count(&self) -> usize {
        self.loaded_tool_names.len()
    }

    /// Serialized schema bytes charged through Tool Search so far this turn.
    pub(crate) fn used_schema_bytes(&self) -> usize {
        self.used_schema_bytes
    }

    /// Admit `discovered` Tool Search hits (in ranking order) into this
    /// turn's exposure and charge the budget. Classification, in order:
    /// missing schema first (`unavailable`), then anything needing no new
    /// exposure (`already_available`: already offered in the current tool
    /// view such as an active group schema, or already charged this turn -
    /// neither consumes budget twice), then the twin budget gates
    /// (`loaded` vs `skipped_budget`). Only `loaded` mutates `self`.
    pub(crate) fn admit(
        &mut self,
        mcp: &McpManager,
        already_offered: &BTreeSet<String>,
        discovered: &[String],
    ) -> ToolSearchAdmission {
        let mut admission = ToolSearchAdmission::default();
        if discovered.is_empty() {
            return admission;
        }
        let resolved = mcp.tool_definitions_for(discovered);
        let mut by_name: HashMap<&str, &Value> = HashMap::new();
        for definition in &resolved {
            if let Some(name) = definition["function"]["name"].as_str() {
                by_name.insert(name, definition);
            }
        }
        for name in discovered {
            let Some(definition) = by_name.get(name.as_str()) else {
                admission.unavailable.push(name.clone());
                continue;
            };
            if already_offered.contains(name) || self.loaded_tool_names.contains(name) {
                admission.already_available.push(name.clone());
                continue;
            }
            let bytes = serde_json::to_vec(definition)
                .map(|body| body.len())
                .unwrap_or(0);
            if self.loaded_tool_names.len() >= MAX_TOOL_SEARCH_EXPOSED_TOOLS_PER_TURN {
                admission.skipped_budget.push(name.clone());
                continue;
            }
            if self.used_schema_bytes + bytes > MAX_TOOL_SEARCH_SCHEMA_BYTES_PER_TURN {
                admission.skipped_budget.push(name.clone());
                continue;
            }
            self.loaded_tool_names.insert(name.clone());
            self.used_schema_bytes += bytes;
            admission.loaded_names.push(name.clone());
            admission.loaded_definitions.push((*definition).clone());
        }
        admission
    }
}

/// One admission decision: what a single `tool_search` result did to the
/// turn's exposure. `loaded_*` is the only part that consumed budget;
/// `already_available` and `unavailable` cost nothing, and `skipped_budget`
/// is what the model must hear about (see [`exposure_budget_note`]).
#[derive(Debug, Default)]
pub(crate) struct ToolSearchAdmission {
    pub loaded_names: Vec<String>,
    pub loaded_definitions: Vec<Value>,
    pub skipped_budget: Vec<String>,
    pub already_available: Vec<String>,
    pub unavailable: Vec<String>,
}

/// Model-readable note for a `tool_search` result that left schemas on the
/// floor. Names the skipped tools (bounded) so the model knows exactly which
/// matches did *not* become callable: without the names, a byte-budget skip
/// can read as contradictory (a nearly empty tool counter next to "budget
/// reached"), and a count-budget skip leaves the model guessing which of the
/// ranked results made the cut. Admission is in ranking order, so narrowing
/// the next search toward the top hits is what recovers them. Only rendered
/// when something was actually skipped, so fully loaded searches pay no
/// extra prompt.
pub(crate) fn exposure_budget_note(
    exposure: &ToolSearchExposure,
    loaded: usize,
    skipped: &[String],
) -> String {
    const MAX_NAMED_SKIPS: usize = 8;
    let mut named = skipped
        .iter()
        .take(MAX_NAMED_SKIPS)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if skipped.len() > MAX_NAMED_SKIPS {
        named.push_str(&format!(", and {} more", skipped.len() - MAX_NAMED_SKIPS));
    }
    format!(
        "Tool Search exposure: loaded {loaded} tool(s) in ranking order; skipped {} ({named}) because the per-turn schema exposure budget was reached ({}/{} tools, {} KiB/{} KiB). Narrow the next search, or use mcp_load_group only if the whole group is intentionally required.",
        skipped.len(),
        exposure.charged_tool_count(),
        MAX_TOOL_SEARCH_EXPOSED_TOOLS_PER_TURN,
        exposure.used_schema_bytes() / 1024,
        MAX_TOOL_SEARCH_SCHEMA_BYTES_PER_TURN / 1024,
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

    fn admit_names(
        exposure: &mut ToolSearchExposure,
        manager: &McpManager,
        names: &[&str],
    ) -> ToolSearchAdmission {
        exposure.admit(
            manager,
            &BTreeSet::new(),
            &names
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>(),
        )
    }

    fn budget_manager(tool_count: usize) -> McpManager {
        let mut manager = McpManager::default();
        for index in 0..tool_count {
            manager.insert_test_tool("bulk", &format!("helper_{index:02}"));
        }
        manager.disable_group("bulk").unwrap();
        manager
    }

    /// Past 32 newly exposed tools the rest is skipped in ranking order,
    /// and the skip list names exactly what did not fit.
    #[test]
    fn admission_stops_at_the_tool_count_budget() {
        let manager = budget_manager(40);
        let mut exposure = ToolSearchExposure::default();
        let discovered: Vec<String> = search(&manager, "bulk", MAX_LIMIT)
            .iter()
            .map(|hit| hit.name.clone())
            .collect();
        assert_eq!(discovered.len(), MAX_LIMIT);

        let first = exposure.admit(&manager, &BTreeSet::new(), &discovered);
        assert_eq!(first.loaded_names.len(), MAX_LIMIT);
        assert!(first.skipped_budget.is_empty());

        let rest: Vec<String> = (0..40)
            .map(|index| format!("mcp__bulk__helper_{index:02}"))
            .filter(|name| !discovered.contains(name))
            .collect();
        assert_eq!(rest.len(), 20);
        let second = exposure.admit(&manager, &BTreeSet::new(), &rest);
        assert_eq!(second.loaded_names.len(), 12, "32 - 20 already charged");
        assert_eq!(second.skipped_budget.len(), 8);
        assert_eq!(exposure.charged_tool_count(), 32);
    }

    /// One schema larger than the whole byte budget never loads, while a
    /// small sibling from the same result still does.
    #[test]
    fn admission_stops_at_the_schema_byte_budget() {
        let mut manager = McpManager::default();
        manager.insert_test_tool_full(
            "big",
            "huge_tool",
            &"x".repeat(110_000),
            json!({"type": "object"}),
        );
        manager.insert_test_tool("big", "note_tool");
        manager.disable_group("big").unwrap();
        let mut exposure = ToolSearchExposure::default();

        let admission = admit_names(
            &mut exposure,
            &manager,
            &["mcp__big__huge_tool", "mcp__big__note_tool"],
        );

        assert_eq!(
            admission.loaded_names,
            vec!["mcp__big__note_tool".to_string()]
        );
        assert_eq!(
            admission.skipped_budget,
            vec!["mcp__big__huge_tool".to_string()]
        );
        assert_eq!(exposure.charged_tool_count(), 1);
        assert!(exposure.used_schema_bytes() < MAX_TOOL_SEARCH_SCHEMA_BYTES_PER_TURN);
    }

    /// Re-searching a charged tool, or one already offered (an active group
    /// schema), costs nothing - and unknown or disconnected names resolve
    /// to `unavailable` instead of consuming budget.
    #[test]
    fn admission_does_not_recharge_duplicates_or_offered_tools() {
        let mut manager = McpManager::default();
        manager.insert_test_tool("github", "search_issues");
        manager.insert_test_tool("github", "create_issue");
        manager.disable_group("github").unwrap();
        let mut exposure = ToolSearchExposure::default();

        let first = admit_names(&mut exposure, &manager, &["mcp__github__search_issues"]);
        assert_eq!(first.loaded_names.len(), 1);
        let bytes = exposure.used_schema_bytes();
        assert!(bytes > 0);

        let second = admit_names(
            &mut exposure,
            &manager,
            &[
                "mcp__github__search_issues",
                "mcp__github__create_issue",
                "mcp__nope__missing",
            ],
        );
        assert_eq!(
            second.already_available,
            vec!["mcp__github__search_issues".to_string()]
        );
        assert_eq!(
            second.loaded_names,
            vec!["mcp__github__create_issue".to_string()]
        );
        assert_eq!(second.unavailable, vec!["mcp__nope__missing".to_string()]);
        assert!(second.skipped_budget.is_empty());

        let offered: BTreeSet<String> = ["mcp__github__create_issue".to_string()].into();
        let bytes_before = exposure.used_schema_bytes();
        let third = exposure.admit(
            &manager,
            &offered,
            &["mcp__github__create_issue".to_string()],
        );
        assert_eq!(
            third.already_available,
            vec!["mcp__github__create_issue".to_string()]
        );
        assert!(third.loaded_names.is_empty());
        assert_eq!(exposure.used_schema_bytes(), bytes_before);
        assert_eq!(exposure.charged_tool_count(), 2);
    }

    /// A disconnect after charging keeps the budget consumed: the names stay
    /// charged even though the schemas no longer resolve.
    #[test]
    fn admission_stays_monotonic_when_schemas_disappear() {
        let mut manager = McpManager::default();
        manager.insert_test_tool("github", "search_issues");
        manager.disable_group("github").unwrap();
        let mut exposure = ToolSearchExposure::default();

        admit_names(&mut exposure, &manager, &["mcp__github__search_issues"]);
        let bytes = exposure.used_schema_bytes();

        manager.disconnect("github").unwrap();
        assert!(
            manager
                .tool_definitions_for(&["mcp__github__search_issues".to_string()])
                .is_empty()
        );

        assert_eq!(exposure.charged_tool_count(), 1);
        assert_eq!(exposure.used_schema_bytes(), bytes);
        let names: Vec<&String> = exposure.names().collect();
        assert_eq!(names, vec!["mcp__github__search_issues"]);
    }
}
