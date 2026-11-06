use super::*;
use dsh_types::observed_output::{ObservedOutput, ObservedStream};
use std::io::Write;

#[tokio::test(flavor = "current_thread")]
async fn parent_capture_drains_blocking_output_without_tokio_progress() {
    let (reader, writer) = super::super::super::cloexec_pipe().unwrap();
    let mut writer = std::fs::File::from(writer);
    let payload = [b"line\n".repeat(300_000), b"tail\0\xff".to_vec()].concat();
    let observer = ObservedOutput::shared(payload.len() + 100);
    let monitor =
        OutputMonitor::new(reader, Some(observer.clone()), ObservedStream::Stdout).unwrap();
    let drain = ConcurrentOutputDrain::start(monitor).unwrap();

    // Deliberately block the test's only runtime thread, as a sync builtin does.
    writer.write_all(&payload).unwrap();
    let mut monitor = drain.finish().unwrap();
    // finish does not require capture EOF and must leave the fragment for Job.
    drop(writer);
    monitor.drain_to_eof().await.unwrap();
    assert_eq!(monitor.captured_output, payload);
    assert_eq!(
        observer.lock().unwrap().snapshot().stdout,
        String::from_utf8_lossy(&payload)
    );
    monitor.drain_to_eof().await.unwrap();
    assert_eq!(monitor.captured_output, payload);
    assert_eq!(
        observer.lock().unwrap().snapshot().stdout,
        String::from_utf8_lossy(&payload)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn parent_capture_finish_preserves_an_open_partial_record() {
    let (reader, writer) = super::super::super::cloexec_pipe().unwrap();
    let mut writer = std::fs::File::from(writer);
    let observer = ObservedOutput::shared(100);
    let monitor =
        OutputMonitor::new(reader, Some(observer.clone()), ObservedStream::Stdout).unwrap();
    let drain = ConcurrentOutputDrain::start(monitor).unwrap();
    writer.write_all(b"partial").unwrap();
    let mut monitor = drain.finish().unwrap();
    assert!(observer.lock().unwrap().snapshot().stdout.is_empty());
    writer.write_all(b"-tail").unwrap();
    drop(writer);
    monitor.drain_to_eof().await.unwrap();
    assert_eq!(monitor.captured_output, b"partial-tail");
    assert_eq!(observer.lock().unwrap().snapshot().stdout, "partial-tail");
}

#[tokio::test(flavor = "current_thread")]
async fn parent_capture_drop_stops_reader_without_producer_eof() {
    for _ in 0..32 {
        let (reader, _writer) = super::super::super::cloexec_pipe().unwrap();
        let monitor = OutputMonitor::new(reader, None, ObservedStream::Stdout).unwrap();
        let drain = ConcurrentOutputDrain::start(monitor).unwrap();
        // Equivalent to an async builtin's future being dropped: no finish/EOF.
        drop(drain);
    }
}
