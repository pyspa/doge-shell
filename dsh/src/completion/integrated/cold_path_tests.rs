use super::*;
use std::sync::mpsc;

#[test]
fn cold_tab_borrows_pending_scan_and_invalidates_empty_result_on_publication() {
    let _guard = crate::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let env = Environment::new_synthetic();
    {
        let mut environment = env.write();
        environment.variable_state.paths = vec![dir.path().display().to_string()];
        environment.isolated_projection = false;
        environment.invocation.interactive = true;
    }
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = env
        .read()
        .start_path_ghost_scan(move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(vec!["zz-cold-ready".into()])
        })
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel();
    env.read().set_path_ghost_notifier(notify_tx.clone());
    let engine = IntegratedCompletionEngine::new(env.clone());
    engine.set_notifier(notify_tx);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let input = "sudo command zz-cold-";
    let first = runtime.block_on(engine.complete(input, input.len(), dir.path(), 50, None));
    let second = runtime.block_on(engine.complete(input, input.len(), dir.path(), 50, None));
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    assert!(first.candidates.is_empty());
    assert!(second.candidates.is_empty());
    assert!(
        notify_rx.try_recv().is_ok(),
        "accepted scan must notify the REPL"
    );
    let ready = runtime.block_on(engine.complete(input, input.len(), dir.path(), 50, None));
    assert!(
        ready
            .candidates
            .iter()
            .any(|candidate| candidate.text == "zz-cold-ready"),
        "cold Tab must not supersede the blocked scanner, and empty cache must expire on publication: {:?}",
        ready.candidates
    );
}
