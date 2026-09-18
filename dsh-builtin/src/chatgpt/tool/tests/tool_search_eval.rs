//! Deterministic retrieval eval for Tool Search ranking.
//!
//! Unit tests pin how the ranker behaves on hand-picked inputs; this module
//! measures whether natural-language requests actually surface the right MCP
//! tool in the Top-N. Fully offline: the catalog is a JSON fixture, ranking
//! goes through the production [`tool_search::search`], and the metrics are
//! pure functions over the ranked name lists. No network, no LLM judge.
//!
//! Run the human-readable report with:
//! `cargo test -p dsh-builtin tool_search_eval_report -- --ignored --nocapture`

use crate::chatgpt::McpManager;
use crate::chatgpt::tool::tool_search;
use serde_json::Value;

const FIXTURE_JSON: &str = include_str!("data/tool_search_eval.json");
/// Retrieval depth for one query: [`tool_search::MAX_LIMIT`] keeps near
/// misses (rank 6-20) visible to MRR instead of scoring them zero.
const RETRIEVAL_LIMIT: usize = tool_search::MAX_LIMIT;
/// How many miss cases the manual report prints for failure analysis.
const REPORT_MISSES: usize = 10;
/// How many actual Top-N names each miss case shows.
const MISS_TOP_N: usize = 3;

// Thresholds are set ~5 points below the measured baseline (see the report
// test output): a real retrieval regression trips them, while a healthy
// rerank that shuffles near-ties stays green. Baseline: Hit@5 = 1.000,
// MRR = 0.951 (45 tools, 66 queries).
const MIN_HIT_AT_5: f64 = 0.95;
const MIN_MRR: f64 = 0.90;

struct FixtureTool {
    server: String,
    group: String,
    tool: String,
    description: String,
    schema: Value,
}

struct FixtureQuery {
    id: String,
    query: String,
    relevant: Vec<String>,
}

fn load_fixture() -> (Vec<FixtureTool>, Vec<FixtureQuery>) {
    let fixture: Value = serde_json::from_str(FIXTURE_JSON).expect("eval fixture parses");
    let tools = fixture["catalog"]
        .as_array()
        .expect("catalog is an array")
        .iter()
        .map(|entry| FixtureTool {
            server: entry["server"].as_str().expect("tool server").to_string(),
            group: entry["group"].as_str().expect("tool group").to_string(),
            tool: entry["tool"].as_str().expect("tool name").to_string(),
            description: entry["description"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            schema: entry["input_schema"].clone(),
        })
        .collect();
    let queries = fixture["queries"]
        .as_array()
        .expect("queries is an array")
        .iter()
        .map(|entry| FixtureQuery {
            id: entry["id"].as_str().expect("query id").to_string(),
            query: entry["query"].as_str().expect("query text").to_string(),
            relevant: entry["relevant"]
                .as_array()
                .expect("relevant is an array")
                .iter()
                .map(|name| name.as_str().expect("relevant name").to_string())
                .collect(),
        })
        .collect();
    (tools, queries)
}

fn build_manager(tools: &[FixtureTool]) -> McpManager {
    let mut manager = McpManager::default();
    for tool in tools {
        manager.insert_test_tool_full(
            &tool.server,
            &tool.tool,
            &tool.description,
            tool.schema.clone(),
        );
        assert_eq!(tool.group, tool.server, "groups are 1:1 with servers");
    }
    manager
}

/// Mean retrieval metrics over per-query ranked name lists. `ranked[i]` is
/// the retrieval for `relevant[i]` in rank order; anything past the
/// retrieval limit counts as absent (MRR contributes 0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EvalMetrics {
    pub queries: usize,
    pub catalog_tools: usize,
    pub hit_at_1: f64,
    pub hit_at_3: f64,
    pub hit_at_5: f64,
    pub recall_at_1: f64,
    pub recall_at_3: f64,
    pub recall_at_5: f64,
    pub mrr: f64,
}

fn compute_metrics(
    ranked: &[Vec<String>],
    relevant: &[Vec<String>],
    queries: usize,
    catalog_tools: usize,
) -> EvalMetrics {
    let mut hit_at_1 = 0usize;
    let mut hit_at_3 = 0usize;
    let mut hit_at_5 = 0usize;
    let mut recall_at_1 = 0.0;
    let mut recall_at_3 = 0.0;
    let mut recall_at_5 = 0.0;
    let mut reciprocal_sum = 0.0;
    for (names, wanted) in ranked.iter().zip(relevant.iter()) {
        // The fixture test guarantees every query names at least one known
        // tool; an empty `wanted` here means a broken caller, not a miss.
        // Fail loudly in debug instead of folding a NaN into the mean.
        debug_assert!(!wanted.is_empty(), "every query needs a relevant tool");
        let first_rank = names
            .iter()
            .position(|name| wanted.iter().any(|name_wanted| name_wanted == name));
        if let Some(rank) = first_rank {
            reciprocal_sum += 1.0 / (rank + 1) as f64;
            if rank < 1 {
                hit_at_1 += 1;
            }
            if rank < 3 {
                hit_at_3 += 1;
            }
            if rank < 5 {
                hit_at_5 += 1;
            }
        }
        for (limit, acc) in [
            (1, &mut recall_at_1),
            (3, &mut recall_at_3),
            (5, &mut recall_at_5),
        ] {
            let found = names
                .iter()
                .take(limit)
                .filter(|name| wanted.iter().any(|name_wanted| name_wanted == *name))
                .count();
            *acc += found as f64 / wanted.len() as f64;
        }
    }
    let total = queries as f64;
    EvalMetrics {
        queries,
        catalog_tools,
        hit_at_1: hit_at_1 as f64 / total,
        hit_at_3: hit_at_3 as f64 / total,
        hit_at_5: hit_at_5 as f64 / total,
        recall_at_1: recall_at_1 / total,
        recall_at_3: recall_at_3 / total,
        recall_at_5: recall_at_5 / total,
        mrr: reciprocal_sum / total,
    }
}

struct EvalMiss {
    id: String,
    query: String,
    relevant: Vec<String>,
    top: Vec<String>,
}

struct EvalReport {
    metrics: EvalMetrics,
    misses: Vec<EvalMiss>,
}

/// Rank every fixture query with the production ranker and score it. Misses
/// are queries with no relevant tool in the Top-5, in fixture order.
fn run_eval() -> EvalReport {
    let (tools, queries) = load_fixture();
    let manager = build_manager(&tools);
    let mut ranked = Vec::with_capacity(queries.len());
    let mut relevant = Vec::with_capacity(queries.len());
    for query in &queries {
        ranked.push(
            tool_search::search(&manager, &query.query, RETRIEVAL_LIMIT)
                .iter()
                .map(|hit| hit.name.clone())
                .collect(),
        );
        relevant.push(query.relevant.clone());
    }
    let metrics = compute_metrics(&ranked, &relevant, queries.len(), tools.len());
    let misses = queries
        .iter()
        .zip(ranked.iter())
        .filter(|(query, names)| {
            !names
                .iter()
                .take(5)
                .any(|name| query.relevant.contains(name))
        })
        .map(|(query, names)| EvalMiss {
            id: query.id.clone(),
            query: query.query.clone(),
            relevant: query.relevant.clone(),
            top: names.iter().take(MISS_TOP_N).cloned().collect(),
        })
        .collect();
    EvalReport { metrics, misses }
}

/// Serialized per-tool definition sizes in bytes, sorted ascending.
fn schema_sizes(tools: &[FixtureTool], manager: &McpManager) -> Vec<usize> {
    let mut sizes: Vec<usize> = tools
        .iter()
        .map(|tool| {
            let name = format!("mcp__{}__{}", tool.server, tool.tool);
            let definitions = manager.tool_definitions_for(&[name]);
            assert_eq!(definitions.len(), 1, "fixture tool must resolve");
            serde_json::to_vec(&definitions[0])
                .expect("definition serializes")
                .len()
        })
        .collect();
    sizes.sort_unstable();
    sizes
}

/// Nearest-rank percentile over a sorted sample.
fn percentile(sorted: &[usize], percent: f64) -> usize {
    debug_assert!(!sorted.is_empty(), "percentile needs a sample");
    let rank = (percent / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len().saturating_sub(1))]
}

#[test]
#[ignore]
fn tool_search_eval_report() {
    let report = run_eval();
    let metrics = report.metrics;
    println!(
        "tool_search eval: {} queries over {} catalog tools",
        metrics.queries, metrics.catalog_tools
    );
    println!(
        "Hit@1 {:.3}  Hit@3 {:.3}  Hit@5 {:.3}",
        metrics.hit_at_1, metrics.hit_at_3, metrics.hit_at_5
    );
    println!(
        "Recall@1 {:.3}  Recall@3 {:.3}  Recall@5 {:.3}",
        metrics.recall_at_1, metrics.recall_at_3, metrics.recall_at_5
    );
    println!("MRR {:.3}", metrics.mrr);

    let (tools, _) = load_fixture();
    let manager = build_manager(&tools);
    let sizes = schema_sizes(&tools, &manager);
    let mean = sizes.iter().sum::<usize>() as f64 / sizes.len() as f64;
    println!(
        "schema bytes: min {}  p50 {}  p95 {}  max {}  mean {:.0}",
        sizes[0],
        percentile(&sizes, 50.0),
        percentile(&sizes, 95.0),
        sizes[sizes.len() - 1],
        mean
    );

    println!(
        "misses (no relevant tool in Top-5): {}",
        report.misses.len()
    );
    for miss in report.misses.iter().take(REPORT_MISSES) {
        println!("MISS [{}]: {:?}", miss.id, miss.query);
        println!("  expected: {}", miss.relevant.join(", "));
        for (index, name) in miss.top.iter().enumerate() {
            println!("  top {}: {name}", index + 1);
        }
    }
}

/// The regression gate that runs in normal CI: floors, not exact ranks, so
/// a healthy rerank that shuffles near-ties stays green while a real
/// retrieval regression fails loudly.
#[test]
fn tool_search_eval_regression() {
    let report = run_eval();
    let metrics = report.metrics;
    assert!(
        metrics.hit_at_5 >= MIN_HIT_AT_5,
        "Hit@5 {:.3} below floor {MIN_HIT_AT_5}",
        metrics.hit_at_5
    );
    assert!(
        metrics.mrr >= MIN_MRR,
        "MRR {:.3} below floor {MIN_MRR}",
        metrics.mrr
    );
    // Same fixture twice is the same numbers: no map-order or time input.
    let again = run_eval().metrics;
    assert_eq!(metrics, again, "eval must be deterministic");
}

#[test]
fn tool_search_eval_fixture_parses() {
    let (tools, queries) = load_fixture();
    assert!(tools.len() >= 40, "need 40+ tools, got {}", tools.len());
    assert!(
        queries.len() >= 60,
        "need 60+ queries, got {}",
        queries.len()
    );
    let catalog: std::collections::BTreeSet<String> = tools
        .iter()
        .map(|tool| format!("mcp__{}__{}", tool.server, tool.tool))
        .collect();
    assert_eq!(catalog.len(), tools.len(), "tool names must be unique");
    for query in &queries {
        assert!(
            !query.relevant.is_empty(),
            "{} has no relevant tool",
            query.id
        );
        for name in &query.relevant {
            assert!(
                catalog.contains(name),
                "{} points at unknown {name}",
                query.id
            );
        }
    }
}

#[test]
fn eval_metrics_match_a_hand_computed_sample() {
    let ranked = vec![
        vec!["a".to_string(), "b".to_string(), "c".to_string()],
        vec!["x".to_string(), "a".to_string(), "b".to_string()],
        vec!["x".to_string(), "y".to_string(), "z".to_string()],
    ];
    let relevant = vec![
        vec!["a".to_string()],
        vec!["a".to_string(), "b".to_string()],
        vec!["a".to_string()],
    ];
    let metrics = compute_metrics(&ranked, &relevant, 3, 4);
    assert_eq!(metrics.hit_at_1, 1.0 / 3.0);
    assert_eq!(metrics.hit_at_3, 2.0 / 3.0);
    assert_eq!(metrics.hit_at_5, 2.0 / 3.0);
    assert_eq!(metrics.recall_at_1, 1.0 / 3.0);
    assert_eq!(metrics.recall_at_3, 2.0 / 3.0);
    assert_eq!(metrics.recall_at_5, 2.0 / 3.0);
    assert_eq!(metrics.mrr, (1.0 + 0.5) / 3.0);
}
