//! Renderer-failure isolation tests for [`OutputMonitor`](super::OutputMonitor).
//!
//! Terminal rendering is best-effort presentation only: once the injected
//! flush closure fails, the monitor disables rendering for the rest of its
//! lifetime (sticky `renderer_failed`), keeps draining/capturing/observing,
//! and still returns `Ok` unless a real read/framing error occurred.

use super::{OutputMonitor, RENDER_FLUSH_BYTES};
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use dsh_types::observed_output::{ObservedStream, SharedOutputObserver};
use std::io::{ErrorKind, Write as _};
use std::time::Duration;

fn unnamed_pipe() -> (std::os::fd::OwnedFd, std::fs::File) {
    let (read, write) = nix::unistd::pipe().expect("pipe");
    (read, std::fs::File::from(write))
}

fn write_to_pipe(writer: &mut std::fs::File, data: &[u8]) {
    writer.write_all(data).expect("write output");
    writer.flush().expect("flush output");
}

fn failing_once_broken_pipe() -> std::io::Error {
    std::io::Error::new(ErrorKind::BrokenPipe, "test renderer failure")
}

#[tokio::test]
async fn output_monitor_completion_renderer_failure_is_sticky_across_running_drains() {
    let observer: SharedOutputObserver = dsh_types::observed_output::ObservedOutput::shared(1024);
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = OutputMonitor::new(read, Some(observer.clone()), ObservedStream::Stdout)
        .expect("create monitor");

    // Poll 1: renderer fails on the first display flush.
    write_to_pipe(&mut writer, b"first\n");
    let mut flush = |_: &mut TerminalRenderer, _: &str| -> Result<()> {
        Err(failing_once_broken_pipe().into())
    };
    monitor
        .drain_available_for_with(Duration::from_millis(5), &mut flush)
        .await
        .expect("renderer failure must not fail the running drain");
    assert!(monitor.renderer_failed);
    assert_eq!(monitor.captured_output, "first\n");
    {
        let observed = observer.lock().expect("observer lock");
        assert_eq!(observed.snapshot().stdout.matches("first\n").count(), 1);
    }

    // Poll 2: renderer must not be retried; capture/observer continue.
    write_to_pipe(&mut writer, b"second\n");
    let mut no_render = |_: &mut TerminalRenderer, _: &str| -> Result<()> {
        panic!("failed renderer must not be retried")
    };
    monitor
        .drain_available_for_with(Duration::from_millis(5), &mut no_render)
        .await
        .expect("second running drain stays Ok without rendering");
    assert!(monitor.renderer_failed);
    assert_eq!(monitor.captured_output, "first\nsecond\n");
    {
        let observed = observer.lock().expect("observer lock");
        let stdout_text = observed.snapshot().stdout;
        assert_eq!(stdout_text.matches("first\n").count(), 1);
        assert_eq!(stdout_text.matches("second\n").count(), 1);
    }
    drop(writer);
}

#[tokio::test]
async fn output_monitor_completion_renderer_failure_to_eof_drains_after_failure() {
    // Payload well over two render-flush thresholds so the failure lands
    // before all bytes are consumed; the rest must still drain.
    let mut payload = Vec::new();
    let mut line_count = 0;
    while payload.len() <= RENDER_FLUSH_BYTES * 2 {
        payload.extend_from_slice(format!("line-{line_count:04}\n").as_bytes());
        line_count += 1;
    }
    let expected = String::from_utf8(payload.clone()).expect("payload is UTF-8");

    let observer: SharedOutputObserver =
        dsh_types::observed_output::ObservedOutput::shared(1024 * 1024);
    let (read, writer) = unnamed_pipe();
    let mut monitor = OutputMonitor::new(read, Some(observer.clone()), ObservedStream::Stdout)
        .expect("create monitor");

    // Concurrent writer: never depend on pipe capacity from the test thread.
    let writer_handle = std::thread::spawn(move || -> std::io::Result<()> {
        let mut writer = writer;
        writer.write_all(&payload)?;
        writer.flush()?;
        Ok(())
    });

    let mut flush_calls = 0;
    let mut flush = |_: &mut TerminalRenderer, _: &str| -> Result<()> {
        flush_calls += 1;
        if flush_calls == 1 {
            return Err(failing_once_broken_pipe().into());
        }
        panic!("failed renderer must not be retried during ToEof");
    };
    monitor
        .drain_to_eof_with(&mut flush)
        .await
        .expect("renderer failure must not fail drain_to_eof");
    writer_handle
        .join()
        .expect("writer thread")
        .expect("writer write");

    assert_eq!(flush_calls, 1);
    assert!(monitor.renderer_failed);
    assert_eq!(monitor.captured_output, expected);
    // The tail proves reading continued after the renderer failed.
    assert!(
        monitor
            .captured_output
            .ends_with(&expected[expected.len() - 64..])
    );
    {
        let observed = observer.lock().expect("observer lock");
        assert_eq!(observed.snapshot().stdout, expected);
    }
}

#[tokio::test]
async fn output_monitor_completion_renderer_failure_ready_now_is_nonfatal() {
    let observer: SharedOutputObserver = dsh_types::observed_output::ObservedOutput::shared(1024);
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = OutputMonitor::new(read, Some(observer.clone()), ObservedStream::Stdout)
        .expect("create monitor");

    // Writer stays open: ReadyNow never waits for EOF.
    write_to_pipe(&mut writer, b"READY\nFRAGMENT");
    let mut flush = |_: &mut TerminalRenderer, _: &str| -> Result<()> {
        Err(failing_once_broken_pipe().into())
    };
    monitor
        .finalize_ready_now_with(&mut flush)
        .expect("renderer failure must not fail ReadyNow");

    assert!(monitor.renderer_failed);
    assert_eq!(monitor.captured_output, "READY\nFRAGMENT");
    assert!(monitor.pending_line.is_empty());
    {
        let observed = observer.lock().expect("observer lock");
        let stdout_text = observed.snapshot().stdout;
        assert_eq!(stdout_text.matches("READY\n").count(), 1);
        assert!(stdout_text.ends_with("FRAGMENT"));
    }

    // Second retirement: renderer still disabled, capture continues.
    write_to_pipe(&mut writer, b"AFTER\n");
    let mut no_render = |_: &mut TerminalRenderer, _: &str| -> Result<()> {
        panic!("failed renderer must not be retried by ReadyNow")
    };
    monitor
        .finalize_ready_now_with(&mut no_render)
        .expect("second ReadyNow stays Ok without rendering");
    assert!(monitor.captured_output.contains("AFTER\n"));
    {
        let observed = observer.lock().expect("observer lock");
        let stdout_text = observed.snapshot().stdout;
        assert_eq!(stdout_text.matches("AFTER\n").count(), 1);
    }
    drop(writer);
}
