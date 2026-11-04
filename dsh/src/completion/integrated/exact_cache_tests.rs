use super::*;

fn engine() -> IntegratedCompletionEngine {
    let environment = Environment::new_synthetic();
    environment.write().variable_state.paths.clear();
    environment
        .write()
        .set_shell_var("DOGESH_COMPLETION_FISH_FALLBACK".into(), "off".into());
    let engine = IntegratedCompletionEngine::new(environment);
    engine.command_completion.lock().add_command(
        serde_json::from_str(r#"{"command":"cacheprobe","global_options":[{"long":"--fresh"}]}"#)
            .unwrap(),
    );
    engine
}
fn candidate(text: &str) -> EnhancedCandidate {
    EnhancedCandidate {
        text: text.into(),
        description: Some("snapshot".into()),
        candidate_type: CandidateType::LongOption,
        priority: 42,
    }
}
#[tokio::test]
async fn exact_snapshot_preserves_framework_order_range_and_deadline() {
    let engine = engine();
    let dir = tempfile::tempdir().unwrap();
    let input = "cacheprobe --";
    let scope = engine.current_cache_scope(dir.path());
    let framework =
        if super::super::default_completion_framework() == CompletionFrameworkKind::Inline {
            CompletionFrameworkKind::Skim
        } else {
            CompletionFrameworkKind::Inline
        };
    engine.store_in_cache(
        scope,
        input,
        &[candidate("--z"), candidate("--a")],
        framework,
    );
    let deadline = engine.cache.scoped_expiration(scope, input).unwrap();
    for _ in 0..3 {
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;
        assert_eq!(result.framework, framework);
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            ["--z", "--a"]
        );
        assert!(
            result
                .candidates
                .iter()
                .all(|c| c.priority == 42 && c.description.as_deref() == Some("snapshot"))
        );
        let range = result.replacement_range.unwrap();
        assert_eq!(
            (range.start, range.end),
            (input.find("--").unwrap(), input.len())
        );
        assert_eq!(engine.cache.scoped_expiration(scope, input), Some(deadline));
    }
}
#[tokio::test]
async fn prefix_only_snapshot_is_discarded_and_normal_collection_runs() {
    let engine = engine();
    let dir = tempfile::tempdir().unwrap();
    let scope = engine.current_cache_scope(dir.path());
    engine.store_in_cache(
        scope,
        "cacheprobe ",
        &[candidate("--stale")],
        CompletionFrameworkKind::Skim,
    );
    let input = "cacheprobe --";
    let result = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    assert!(result.candidates.iter().any(|c| c.text == "--fresh"));
    assert!(result.candidates.iter().all(|c| c.text != "--stale"));
}
#[tokio::test]
async fn expired_and_other_scope_snapshots_recollect_instead_of_reusing() {
    let mut engine = engine();
    let dir = tempfile::tempdir().unwrap();
    let input = "cacheprobe --";
    let scope = engine.current_cache_scope(dir.path());
    engine.store_in_cache(
        scope.wrapping_add(1),
        input,
        &[candidate("--wrong-scope")],
        CompletionFrameworkKind::Skim,
    );
    let result = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    assert!(result.candidates.iter().any(|c| c.text == "--fresh"));
    assert!(result.candidates.iter().all(|c| c.text != "--wrong-scope"));
    engine.cache = CompletionCache::new(Duration::ZERO);
    engine
        .cache
        .set_scoped(scope, input.into(), vec![candidate("--expired")]);
    let result = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    assert!(result.candidates.iter().any(|c| c.text == "--fresh"));
    assert!(result.candidates.iter().all(|c| c.text != "--expired"));
}
#[tokio::test]
async fn exact_empty_snapshot_is_a_hit_but_special_tokens_still_bypass_it() {
    let engine = engine();
    let dir = tempfile::tempdir().unwrap();
    let scope = engine.current_cache_scope(dir.path());
    let input = "cacheprobe --";
    engine.cache.set_scoped(scope, input.into(), Vec::new());
    assert!(
        engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await
            .candidates
            .is_empty()
    );
    engine
        .environment
        .write()
        .set_shell_var("CACHE_PROBE_VAR".into(), "value".into());
    let input = "cacheprobe $CACHE_PROBE_";
    let scope = engine.current_cache_scope(dir.path());
    engine
        .cache
        .set_scoped(scope, input.into(), vec![candidate("--stale")]);
    let result = engine
        .complete(input, input.len(), dir.path(), 50, None)
        .await;
    assert!(
        result
            .candidates
            .iter()
            .any(|c| c.text == "$CACHE_PROBE_VAR")
    );
    assert!(result.candidates.iter().all(|c| c.text != "--stale"));
}
