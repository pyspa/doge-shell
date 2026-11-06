//! Command completion limits, runtime environment, cancellation, and event replay.
use super::*;
use crossterm::event::{KeyEvent, KeyEventState};
use std::time::Instant;

#[test]
fn public_legacy_command_api_is_synchronous_and_bounded() {
    let started = Instant::now();
    let result = crate::completion::selection::completion_from_cmd(
        "sleep 3; printf late".into(),
        None,
        CompletionFrameworkKind::Inline,
    );
    assert_eq!(result, CompletionSelection::None);
    assert!(started.elapsed() < Duration::from_millis(2500));
    assert_eq!(
        crate::completion::selection::completion_from_cmd(
            "printf ' legacy_value\\n'".into(),
            None,
            CompletionFrameworkKind::Inline,
        ),
        CompletionSelection::Selected("legacy_value".into()),
    );
}

#[test]
fn legacy_command_rejects_stdout_over_the_existing_limit() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("over-limit.txt");
    std::fs::write(&output, vec![b'x'; subprocess::MAX_STDOUT_BYTES + 1]).unwrap();
    let command = subprocess::shell_command(&format!(
        "cat {}",
        subprocess::shell_quote(output.to_str().unwrap())
    ));
    let error = collect_candidates(command, &|| false).unwrap_err();
    assert!(error.to_string().contains("stdout limit"), "{error}");
}

#[tokio::test]
async fn legacy_command_runtime_uses_cwd_and_only_exported_variables() {
    let root = tempfile::tempdir().unwrap();
    let environment = crate::environment::Environment::new_synthetic();
    environment
        .write()
        .set_and_export_shell_var("LEGACY_EXPORTED".into(), "logical".into());
    environment
        .write()
        .set_shell_var("LEGACY_UNEXPORTED".into(), "secret".into());
    let runtime = environment
        .read()
        .command_runtime_snapshot(root.path().to_path_buf());
    let result = complete_with_runtime(
        "printf '%s:%s:' \"$LEGACY_EXPORTED\" \"${LEGACY_UNEXPORTED-unset}\"; pwd -P".into(),
        runtime,
        None,
        "",
        "",
        CompletionFrameworkKind::Inline,
    )
    .await;
    assert_eq!(
        result,
        CompletionSelection::Selected(format!(
            "logical:unset:{}",
            root.path().canonicalize().unwrap().display(),
        ))
    );
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_wait_yields_and_cancels_while_the_helper_is_running() {
    for event in [
        key(KeyCode::Esc, KeyModifiers::NONE),
        key(KeyCode::Char('c'), KeyModifiers::CONTROL),
    ] {
        let started = Instant::now();
        let task = CommandTask::start(subprocess::shell_command("sleep 3; printf late"));
        let events = Box::pin(futures::stream::once(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(event)
        }));
        let result = wait_for_command(task, events).await;
        assert!(matches!(
            result,
            CommandResult::Interrupted(CompletionSelection::Cancelled)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

#[tokio::test]
async fn legacy_wait_returns_the_original_editing_event() {
    for event in [
        key(KeyCode::Enter, KeyModifiers::NONE),
        key(KeyCode::Backspace, KeyModifiers::NONE),
        key(KeyCode::Left, KeyModifiers::NONE),
        key(KeyCode::Char('x'), KeyModifiers::ALT),
        Event::Paste("two\nlines".into()),
        Event::Resize(91, 27),
    ] {
        let task = CommandTask::start(subprocess::shell_command("sleep 3"));
        let result = wait_for_command(task, futures::stream::iter([Ok(event.clone())])).await;
        let CommandResult::Interrupted(selection) = result else {
            panic!("event was not replayed")
        };
        assert_eq!(selection, CompletionSelection::Event(event));
    }
}

#[tokio::test]
async fn legacy_wait_ignores_key_release_without_losing_the_next_key() {
    let release = Event::Key(KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    });
    let event = key(KeyCode::Enter, KeyModifiers::NONE);
    let task = CommandTask::start(subprocess::shell_command("sleep 3"));
    let result = wait_for_command(
        task,
        futures::stream::iter([Ok(release), Ok(event.clone())]),
    )
    .await;
    let CommandResult::Interrupted(selection) = result else {
        panic!("next key was lost")
    };
    assert_eq!(selection, CompletionSelection::Event(event));
}

#[tokio::test]
async fn dropping_a_legacy_wait_signals_its_blocking_worker() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let (done, finished) = tokio::sync::oneshot::channel();
    let handle = tokio::task::spawn_blocking(move || {
        while !worker_cancelled.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = done.send(());
        Ok(None)
    });
    let task = CommandTask { cancelled, handle };
    let mut wait = Box::pin(wait_for_command(
        task,
        futures::stream::pending::<io::Result<Event>>(),
    ));
    tokio::select! {
        _ = &mut wait => panic!("worker completed without cancellation"),
        _ = tokio::task::yield_now() => {}
    }
    drop(wait);
    tokio::time::timeout(Duration::from_secs(1), finished)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn legacy_cancellation_kills_descendants_before_the_editor_resumes() {
    let _guard = subprocess::external_process_test_guard();
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join("ready");
    let escaped = root.path().join("escaped");
    let mut command = subprocess::shell_command(
        "(sleep 0.5; printf escaped > escaped) & printf ready > ready; wait",
    );
    command.current_dir(root.path());
    let started = Instant::now();
    let result = collect_candidates(command, &|| ready.exists()).unwrap();
    assert!(result.is_none());
    assert!(started.elapsed() < Duration::from_secs(1));
    std::thread::sleep(Duration::from_millis(650));
    assert!(
        !escaped.exists(),
        "cancelled descendant escaped its process group"
    );
}

#[test]
fn legacy_cancellation_interrupts_a_continuous_stdout_producer() {
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join("ready");
    let mut command = subprocess::shell_command("printf ready > ready; while :; do printf x; done");
    command.current_dir(root.path());
    assert!(
        collect_candidates(command, &|| ready.exists())
            .unwrap()
            .is_none()
    );
}
