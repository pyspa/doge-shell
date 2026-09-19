use super::*;

/// The registry is process-wide and `cancel_all`/`shutdown` reach every
/// job in it, so these tests must not run beside anything that starts one.
///
/// Deliberately the *same* lock the `execute` tests take rather than one of
/// its own: two locks would serialize each group internally while still
/// letting a `shutdown()` here kill a job an `execute` test was waiting on.
fn guard() -> MutexGuard<'static, ()> {
    let guard = crate::chatgpt::tool::execute::tests::env_lock();
    shutdown();
    guard
}

/// Start a job printing `script`'s stdout and wait until it leaves
/// `running`, so live-echo tests never race the worker thread.
fn finished_output_job(session: &str, script: &str) -> String {
    set_session(session);
    let mut builder = Command::new("sh");
    builder.args(["-c", script]);
    let id = start(builder, script, None, Duration::from_secs(60)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while with(|jobs| jobs.snapshot(&id, 0, 0)).unwrap()["status"] == "running" {
        assert!(Instant::now() < deadline, "job {id} never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
    id
}

fn echo_meta(id: &str) -> (usize, usize, usize, bool) {
    with(|jobs| {
        let meta = jobs.meta.get(id).expect("job meta");
        (
            meta.echoed_stdout,
            meta.echoed_stderr,
            meta.echoed_total,
            meta.echo_suppressed,
        )
    })
}

/// Under the budget everything is echoed and nothing is suppressed.
#[test]
fn live_echo_under_the_limit_returns_everything() {
    let _lock = guard();

    let id = finished_output_job("echo-a", "printf '12345678'");
    let echo = with(|jobs| jobs.take_echo_with_limit(&id, 16)).expect("echo");
    assert_eq!(echo.0, b"12345678");
    assert!(echo.1.is_empty());
    assert!(!echo.2);

    let (_, _, total, suppressed) = echo_meta(&id);
    assert_eq!(total, 8);
    assert!(!suppressed);

    shutdown();
}

/// The regression that stalled CI: one poll delivering more than the whole
/// budget must not hand the whole chunk to the terminal.
#[test]
fn live_echo_caps_a_single_oversized_poll() {
    let _lock = guard();

    let id = finished_output_job(
        "echo-b",
        "printf '0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF'",
    );
    let echo = with(|jobs| jobs.take_echo_with_limit(&id, 16)).expect("echo");
    let live_len = echo.0.len() + echo.1.len();
    assert!(
        live_len <= 16,
        "a 64-byte poll echoed {live_len} bytes with a 16-byte budget"
    );
    assert_eq!(live_len, 16);
    assert!(echo.2, "the first truncation must report suppression");

    let (echoed_stdout, _, total, suppressed) = echo_meta(&id);
    assert_eq!(total, 16, "echoed_total counts live bytes only");
    assert!(total <= 16);
    assert!(suppressed);
    // Offsets still advanced past the discarded bytes, so the next poll
    // does not re-read the same chunk.
    assert_eq!(echoed_stdout, 64);

    shutdown();
}

/// After suppression there is no more live output and no second notice.
#[test]
fn live_echo_after_suppression_stays_quiet() {
    let _lock = guard();

    let id = finished_output_job("echo-c", "printf '0123456789ABCDEF0123456789ABCDEF'");
    let first = with(|jobs| jobs.take_echo_with_limit(&id, 16)).expect("first echo");
    assert!(first.2);

    let second = with(|jobs| jobs.take_echo_with_limit(&id, 16));
    assert!(second.is_none(), "suppressed jobs echo nothing more");

    // The injected writer path reports the notice exactly once: the first
    // call wrote it, the second writes nothing at all.
    let mut out = Vec::new();
    let mut err = Vec::new();
    with(|jobs| jobs.echo_pending_to_with_limit(&id, &mut out, &mut err, 16));
    assert!(out.is_empty());
    assert!(err.is_empty(), "notice must not repeat: {err:?}");

    shutdown();
}

/// The budget covers stdout and stderr together, spending stdout first.
#[test]
fn live_echo_caps_stdout_and_stderr_together() {
    let _lock = guard();

    let id = finished_output_job("echo-d", "printf '123456789012'; printf 'ABCDEFGHIJKL' >&2");
    let echo = with(|jobs| jobs.take_echo_with_limit(&id, 16)).expect("echo");
    let live_len = echo.0.len() + echo.1.len();
    assert!(live_len <= 16, "combined live bytes exceeded the budget");
    assert_eq!(live_len, 16);
    assert_eq!(echo.0.len(), 12);
    assert_eq!(echo.1.len(), 4);
    assert!(echo.2);

    let (_, _, total, suppressed) = echo_meta(&id);
    assert_eq!(total, 16);
    assert!(suppressed);

    shutdown();
}

/// Exactly at the budget nothing is suppressed; one byte more is.
#[test]
fn live_echo_at_the_exact_boundary_does_not_suppress() {
    let _lock = guard();

    let exact = finished_output_job("echo-e1", "printf '1234567890123456'");
    let echo = with(|jobs| jobs.take_echo_with_limit(&exact, 16)).expect("echo");
    assert_eq!(echo.0.len(), 16);
    assert!(!echo.2);
    let (_, _, total, suppressed) = echo_meta(&exact);
    assert_eq!(total, 16);
    assert!(!suppressed);
    // No new data afterwards: still no suppression notice.
    let again = with(|jobs| jobs.take_echo_with_limit(&exact, 16)).expect("re-poll");
    assert!(!again.2);
    assert!(again.0.is_empty() && again.1.is_empty());

    let over = finished_output_job("echo-e2", "printf '12345678901234567'");
    let echo = with(|jobs| jobs.take_echo_with_limit(&over, 16)).expect("echo");
    assert_eq!(echo.0.len() + echo.1.len(), 16);
    assert!(echo.2);

    shutdown();
}

/// The injected-writer path caps terminal bytes and prints the notice once.
#[test]
fn live_echo_writer_injection_caps_and_notifies_once() {
    let _lock = guard();

    let id = finished_output_job(
        "echo-f",
        "printf '0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF'",
    );
    let mut out = Vec::new();
    let mut err = Vec::new();
    with(|jobs| jobs.echo_pending_to_with_limit(&id, &mut out, &mut err, 16));

    assert_eq!(out.len(), 16, "live stdout exceeded the budget");
    let err_text = String::from_utf8_lossy(&err);
    assert_eq!(
        err_text.matches("live output suppressed").count(),
        1,
        "suppression notice must print exactly once: {err_text:?}"
    );

    let mut out2 = Vec::new();
    let mut err2 = Vec::new();
    with(|jobs| jobs.echo_pending_to_with_limit(&id, &mut out2, &mut err2, 16));
    assert!(out2.is_empty());
    assert!(err2.is_empty());

    shutdown();
}
