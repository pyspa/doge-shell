use super::*;

fn candidate(text: &str, priority: u32, kind: CandidateType) -> EnhancedCandidate {
    EnhancedCandidate {
        text: text.into(),
        priority,
        candidate_type: kind,
        description: None,
    }
}

fn signature(values: &[EnhancedCandidate]) -> Vec<(&str, u32, &CandidateType, Option<&str>)> {
    values
        .iter()
        .map(|v| {
            (
                v.text.as_str(),
                v.priority,
                &v.candidate_type,
                v.description.as_deref(),
            )
        })
        .collect()
}

fn assert_same_first(engine: &IntegratedCompletionEngine, candidates: Vec<EnhancedCandidate>) {
    let full = engine.deduplicate_and_sort(candidates.clone(), usize::MAX, None, None);
    let first = engine.deduplicate_and_sort(candidates, 1, None, None);
    assert_eq!(signature(&first), signature(&full[..full.len().min(1)]));
}

#[test]
fn single_candidate_preserves_stable_ties_and_directory_dedup() {
    let engine = IntegratedCompletionEngine::new(Environment::new());
    let mut first = candidate("same", 100, CandidateType::Generic);
    first.description = Some("first source wins ties".into());
    assert_same_first(&engine, vec![]);
    assert_same_first(
        &engine,
        vec![
            first.clone(),
            candidate("same", 100, CandidateType::Generic),
        ],
    );
    assert_same_first(
        &engine,
        vec![
            candidate("dir/", 10, CandidateType::Directory),
            candidate("dir", 20, CandidateType::Directory),
            candidate("dir", 20, CandidateType::Generic),
            first,
        ],
    );
}

proptest::proptest! {
    #[test]
    fn single_candidate_matches_full_order(
        values in proptest::collection::vec((0u32..20, 0u8..6, 0u8..5), 0..200)
    ) {
        let engine = IntegratedCompletionEngine::new(Environment::new());
        let candidates = values.into_iter().enumerate().map(|(index, (priority, text, kind))| {
            let kind = match kind {
                0 => CandidateType::File,
                1 => CandidateType::Directory,
                2 => CandidateType::Generic,
                3 => CandidateType::SubCommand,
                _ => CandidateType::LongOption,
            };
            let mut value = candidate(&format!("value{}/{}", text / 2, if text % 2 == 0 { "" } else { "x" }), priority, kind);
            value.description = Some(index.to_string());
            value
        }).collect();
        assert_same_first(&engine, candidates);
    }
}

#[test]
fn single_candidate_keeps_history_boost_and_saturation() {
    let engine = IntegratedCompletionEngine::new(Environment::new());
    let mut history = crate::history::History::new();
    history
        .write_batch(vec![("git preferred preferred".into(), 1)])
        .unwrap();
    let history = Arc::new(parking_lot::Mutex::new(history));
    for priority in [0, u32::MAX - 1] {
        let values = vec![
            candidate("other", 20, CandidateType::Generic),
            candidate("preferred", priority, CandidateType::Generic),
        ];
        let all = engine.deduplicate_and_sort(values.clone(), 20, Some(&history), Some("git"));
        let one = engine.deduplicate_and_sort(values, 1, Some(&history), Some("git"));
        assert_eq!(signature(&one), signature(&all[..1]));
        assert_eq!(one[0].text, "preferred");
    }
}

/// Run with --release --ignored --nocapture --test-threads=1. No timing gate in CI.
#[test]
#[ignore]
fn ghost_latency_probe() {
    use std::{hint::black_box, time::Instant};
    for size in [30, 10_000] {
        let engine = IntegratedCompletionEngine::new(Environment::new());
        let choices: Vec<_> = (0..size)
            .rev()
            .map(|i| format!("candidate{i:05}"))
            .collect();
        let definition = serde_json::from_value(serde_json::json!({
            "command": "latency-fixture",
            "arguments": [{"name": "choice", "type": {"type": "Choice", "data": choices}}]
        }))
        .unwrap();
        engine.command_completion.lock().add_command(definition);
        let cwd = tempfile::tempdir().unwrap();
        let input = "latency-fixture c";
        assert_eq!(
            engine
                .ghost_completion(input, input.len(), cwd.path(), None)
                .as_deref(),
            Some("latency-fixture candidate00000")
        );
        let mut samples = Vec::new();
        for _ in 0..21 {
            let start = Instant::now();
            for _ in 0..100 {
                black_box(engine.ghost_completion(black_box(input), input.len(), cwd.path(), None));
            }
            samples.push(start.elapsed().as_nanos() / 100);
        }
        samples.sort();
        println!(
            "ghost candidates={size}: median={}ns p95={}ns",
            samples[10], samples[19]
        );
    }
}
