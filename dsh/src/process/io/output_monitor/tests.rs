use super::{OutputMonitor, append_output_chunk};
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use dsh_types::observed_output::{ObservedStream, SharedOutputObserver};
use std::io::Write as _;
use std::os::fd::IntoRawFd as _;
use std::os::unix::io::FromRawFd as _;

/// Write bytes to a pipe writer without closing it, so the reader
/// observes data but neither newline/EOF completion nor writer close.
fn write_to_pipe(writer: &mut std::fs::File, data: &[u8]) {
    writer.write_all(data).expect("write output");
    writer.flush().expect("flush output");
}

fn unnamed_pipe() -> (std::os::fd::OwnedFd, std::fs::File) {
    let (read, write) = nix::unistd::pipe().expect("pipe");
    // `File` borrows nothing: the writer stays open until the caller
    // drops it, so tests control EOF explicitly.
    let writer = std::fs::File::from(write);
    (read, writer)
}

fn stdout_monitor(read: std::os::fd::OwnedFd) -> OutputMonitor {
    OutputMonitor::new(read, None, ObservedStream::Stdout).expect("create monitor")
}

#[test]
fn append_output_chunk_prefixes_only_first_chunk() {
    let mut started = false;
    let mut buffer = Vec::new();

    append_output_chunk(&mut started, &mut buffer, b"first\n");
    append_output_chunk(&mut started, &mut buffer, b"second\n");

    assert_eq!(buffer, b"\r\nfirst\nsecond\n");
}

#[test]
fn append_output_chunk_keeps_payload_unchanged() {
    let mut started = false;
    let mut buffer = Vec::new();

    append_output_chunk(
        &mut started,
        &mut buffer,
        "\u{1b}[31mred\u{1b}[0m\n".as_bytes(),
    );

    assert_eq!(buffer, "\r\n\u{1b}[31mred\u{1b}[0m\n".as_bytes());
}

#[tokio::test]
async fn output_monitor_drain_to_eof_waits_for_late_output() {
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let mut monitor = stdout_monitor(read);
    let write_fd = write.into_raw_fd();

    // A ready-now poll observes nothing; the writer emits afterwards and
    // `drain_to_eof` still captures it. The channel orders the late write
    // after the poll without any timing assumption.
    monitor.drain_ready_now().expect("ready-now empty");
    assert!(monitor.captured_output.is_empty());
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let writer = std::thread::spawn(move || {
        go_rx.recv().expect("go signal");
        let mut file = unsafe { std::fs::File::from_raw_fd(write_fd) };
        file.write_all(b"late output").expect("write output");
    });

    go_tx.send(()).expect("signal writer");
    monitor.drain_to_eof().await.expect("drain to eof");
    writer.join().expect("writer thread");
    assert_eq!(monitor.captured_output.as_slice(), b"late output");
}

/// An open pipe with no data and no EOF ends the running drain via
/// `WouldBlock`. The API is sync, so returning at all is the wait-free
/// contract: no wall-clock assertion is needed.
#[tokio::test]
async fn output_monitor_running_ready_now_empty_open_pipe() {
    let (read, _writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    monitor.drain_ready_now().expect("empty ready-now drain");
    assert!(monitor.captured_output.is_empty());
    assert!(monitor.pending_line.is_empty());
}

/// A newline-terminated record is recovered with the writer still open
/// (no EOF), exactly once.
#[tokio::test]
async fn output_monitor_running_ready_now_reads_buffered_line() {
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    write_to_pipe(&mut writer, b"READY\n");
    monitor.drain_ready_now().expect("ready-now drain");

    assert_eq!(monitor.captured_output.as_slice(), b"READY\n");
    drop(writer);
}

#[tokio::test]
async fn output_monitor_running_ready_now_preserves_partial_line() {
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    // Partial line without newline or EOF: the ready-now drain keeps it
    // without capturing it, and without losing it.
    write_to_pipe(&mut writer, b"PARTIAL");
    monitor.drain_ready_now().expect("ready-now drain");
    assert!(monitor.captured_output.is_empty());

    // Complete the line and close: the whole record must arrive exactly once.
    write_to_pipe(&mut writer, b"-TAIL\n");
    drop(writer);
    monitor.drain_ready_now().expect("ready-now tail");
    assert_eq!(monitor.captured_output.as_slice(), b"PARTIAL-TAIL\n");
}

#[tokio::test]
async fn output_monitor_running_ready_now_no_newline_published_at_eof() {
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    // `<(printf ...)` shape: bytes with no newline stay pending while the
    // writer is open, then publish at EOF.
    write_to_pipe(&mut writer, b"NO-NEWLINE");
    monitor.drain_ready_now().expect("ready-now drain");
    assert!(monitor.captured_output.is_empty());

    drop(writer);
    monitor.drain_ready_now().expect("ready-now eof");
    assert_eq!(monitor.captured_output.as_slice(), b"NO-NEWLINE");
}

#[tokio::test]
async fn output_monitor_running_ready_now_resume_does_not_duplicate() {
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    write_to_pipe(&mut writer, b"first-");
    monitor.drain_ready_now().expect("ready-now drain");
    assert!(monitor.captured_output.is_empty());

    write_to_pipe(&mut writer, b"second-");
    monitor.drain_ready_now().expect("ready-now drain");
    assert!(monitor.captured_output.is_empty());

    write_to_pipe(&mut writer, b"third\n");
    drop(writer);
    monitor.drain_ready_now().expect("ready-now tail");
    assert_eq!(monitor.captured_output.as_slice(), b"first-second-third\n");
}

#[tokio::test]
async fn output_monitor_running_ready_now_preserves_split_utf8() {
    let observer: SharedOutputObserver = dsh_types::observed_output::ObservedOutput::shared(1024);
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = OutputMonitor::new(read, Some(observer.clone()), ObservedStream::Stdout)
        .expect("create monitor");

    // "あ" is E3 81 82 in UTF-8; split it across the ready-now boundary
    // so a per-chunk lossy conversion would emit U+FFFD. The record
    // completes only after the second drain, so both raw capture and the
    // observer must hold the intact character.
    write_to_pipe(&mut writer, &[0xE3]);
    monitor.drain_ready_now().expect("ready-now drain");
    assert!(monitor.captured_output.is_empty());

    write_to_pipe(&mut writer, &[0x81, 0x82, b'\n']);
    drop(writer);
    monitor.drain_ready_now().expect("ready-now tail");
    assert_eq!(monitor.captured_output.as_slice(), "あ\n".as_bytes());
    let observed = observer.lock().expect("observer lock");
    assert_eq!(observed.snapshot().stdout, "あ\n");
}

/// A small budget stops mid-stream without publishing the pending
/// fragment; later drains recover the rest exactly once with no loss or
/// duplication.
#[tokio::test]
async fn output_monitor_running_ready_now_byte_budget_is_fair() {
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    let mut payload = Vec::new();
    for _ in 0..512 {
        payload.extend_from_slice(b"A\n");
    }
    write_to_pipe(&mut writer, &payload);

    let mut flush = OutputMonitor::flush_terminal;
    let outcome = monitor
        .drain_ready_now_with_budget(1024, &mut flush)
        .expect("budgeted drain");
    assert!(outcome.budget_exhausted);
    assert!(outcome.bytes_read <= 1024);
    assert!(!outcome.eof);

    drop(writer);
    monitor.drain_ready_now().expect("recover remainder");
    let expected = "A\n".repeat(512);
    assert_eq!(monitor.captured_output.as_slice(), expected.as_bytes());
}

/// Binary-safe contract: an invalid-UTF-8 record is preserved byte-exact
/// in the raw capture (old bug: the record disappeared), and later valid
/// output still arrives instead of stalling.
#[tokio::test]
async fn output_monitor_invalid_utf8_does_not_stall_later_output() {
    let observer: SharedOutputObserver = dsh_types::observed_output::ObservedOutput::shared(1024);
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = OutputMonitor::new(read, Some(observer.clone()), ObservedStream::Stdout)
        .expect("create monitor");

    let payload = b"\xff\nafter\n";
    write_to_pipe(&mut writer, payload);
    drop(writer);
    monitor.drain_to_eof().await.expect("drain to eof");
    // Raw authority keeps every byte; the text projection replaces.
    assert_eq!(monitor.captured_output.as_slice(), payload);
    let observed = observer.lock().expect("observer lock");
    let expected_text = String::from_utf8_lossy(payload).into_owned();
    assert_eq!(observed.snapshot().stdout, expected_text);
}

#[tokio::test]
async fn output_monitor_completion_fd_is_nonblocking() {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};

    let (read, _writer) = nix::unistd::pipe().expect("pipe");
    let monitor = stdout_monitor(read);
    let flags = fcntl(monitor.inner.get_ref(), FcntlArg::F_GETFL).expect("F_GETFL");
    assert!(
        OFlag::from_bits_truncate(flags).contains(OFlag::O_NONBLOCK),
        "OutputMonitor must own an O_NONBLOCK reader, flags: {flags:#x}"
    );
}

/// Reconciliation with no new bytes returns immediately: the writer is
/// still open (no EOF), yet `finalize_ready_now` is sync and cannot
/// wait on anything.
#[tokio::test]
async fn output_monitor_completion_would_block_is_terminal() {
    let (read, _writer) = nix::unistd::pipe().expect("pipe");
    let mut monitor = stdout_monitor(read);

    monitor
        .finalize_ready_now()
        .expect("ready-now with no data is Ok");
    assert!(monitor.captured_output.is_empty());
}

/// Basic completion: a newline-terminated record is recovered with the
/// writer still open (no EOF), exactly once.
#[tokio::test]
async fn output_monitor_completion_ready_now_basic() {
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    write_to_pipe(&mut writer, b"READY\n");
    monitor.finalize_ready_now().expect("ready-now drain");

    assert_eq!(monitor.captured_output.as_slice(), b"READY\n");
    // The writer stays open: this never waited for EOF.
    drop(writer);
}

/// The pass-4/pass-5 race in miniature: a poll observes nothing, the
/// child writes afterwards, and completion reconciliation must still
/// recover those bytes instead of dropping the monitor.
#[tokio::test]
async fn output_monitor_completion_race_late_write_after_poll() {
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    monitor.drain_ready_now().expect("pass-4 style poll");
    assert!(monitor.captured_output.is_empty());

    // Written after the poll, before completion is observed.
    write_to_pipe(&mut writer, b"LATE\n");
    monitor.finalize_ready_now().expect("pass-5 style finalize");

    assert_eq!(monitor.captured_output.as_slice(), b"LATE\n");
    drop(writer);
}

/// A partial line held across the running drain (no newline, no EOF,
/// writer open) is published at retirement: monitor ownership ends
/// here, so keeping it would lose it with the monitor.
#[tokio::test]
async fn output_monitor_completion_prior_partial_no_newline() {
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    write_to_pipe(&mut writer, b"PARTIAL");
    monitor
        .drain_ready_now()
        .expect("running drain holds the partial line");
    assert!(monitor.captured_output.is_empty());

    monitor
        .finalize_ready_now()
        .expect("retirement publishes it");
    assert_eq!(monitor.captured_output.as_slice(), b"PARTIAL");
    assert!(monitor.pending_line.is_empty());
    drop(writer);
}

/// A prior fragment plus a late tail join into one record, exactly once.
#[tokio::test]
async fn output_monitor_completion_partial_plus_late_tail() {
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    write_to_pipe(&mut writer, b"PART");
    monitor.drain_ready_now().expect("ready-now drain");
    assert!(monitor.captured_output.is_empty());

    write_to_pipe(&mut writer, b"IAL\n");
    monitor.finalize_ready_now().expect("ready-now drain");
    assert_eq!(monitor.captured_output.as_slice(), b"PARTIAL\n");
    drop(writer);
}

/// A multi-byte character split across the ready-now boundary is only
/// projected once complete: raw capture and observer both hold it intact.
#[tokio::test]
async fn output_monitor_completion_split_utf8() {
    let observer: SharedOutputObserver = dsh_types::observed_output::ObservedOutput::shared(1024);
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = OutputMonitor::new(read, Some(observer.clone()), ObservedStream::Stdout)
        .expect("create monitor");

    write_to_pipe(&mut writer, &[0xE3]);
    monitor.drain_ready_now().expect("ready-now drain");
    assert!(monitor.captured_output.is_empty());

    write_to_pipe(&mut writer, &[0x81, 0x82, b'\n']);
    monitor.finalize_ready_now().expect("ready-now drain");
    assert_eq!(monitor.captured_output.as_slice(), "あ\n".as_bytes());
    let observed = observer.lock().expect("observer lock");
    assert_eq!(observed.snapshot().stdout, "あ\n");
    drop(writer);
}

/// One raw `read` returning several lines plus a trailing fragment
/// publishes every line and the fragment (writer still open, no EOF).
#[tokio::test]
async fn output_monitor_completion_multiple_lines_one_read() {
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    write_to_pipe(&mut writer, b"one\ntwo\nthree");
    monitor.finalize_ready_now().expect("ready-now drain");

    assert_eq!(monitor.captured_output.as_slice(), b"one\ntwo\nthree");
    drop(writer);
}

/// Binary-safe contract: an invalid record read alongside a valid one is
/// preserved byte-exact (old bug: only the valid record survived).
#[tokio::test]
async fn output_monitor_completion_invalid_utf8_keeps_valid_record() {
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    let payload = b"\xff\nafter\n";
    write_to_pipe(&mut writer, payload);
    monitor
        .finalize_ready_now()
        .expect("invalid UTF-8 is data, not a drain error");

    assert_eq!(monitor.captured_output.as_slice(), payload);
    drop(writer);
}

/// Capture, renderer prefix, and observer all observe the retired bytes
/// exactly once — even for a fragment with no newline and no EOF.
#[tokio::test]
async fn output_monitor_completion_observer_exactly_once() {
    let observer: SharedOutputObserver = dsh_types::observed_output::ObservedOutput::shared(1024);
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = OutputMonitor::new(read, Some(observer.clone()), ObservedStream::Stdout)
        .expect("create monitor");

    write_to_pipe(&mut writer, b"READY\nFRAGMENT");
    monitor.finalize_ready_now().expect("ready-now drain");

    assert_eq!(monitor.captured_output.as_slice(), b"READY\nFRAGMENT");
    let observed = observer.lock().expect("observer lock");
    let stdout_text = observed.snapshot().stdout;
    assert_eq!(stdout_text.matches("READY\n").count(), 1);
    assert!(stdout_text.ends_with("FRAGMENT"));
    drop(writer);
}

/// The first-output `\r\n` prefix fires once on the retirement path
/// too: the `outputed` flag flips on the first retired record and stays
/// set, so the display buffer keeps its exactly-once prefix contract.
#[tokio::test]
async fn output_monitor_completion_first_output_prefix_once() {
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    assert!(!monitor.outputed);
    write_to_pipe(&mut writer, b"first\n");
    monitor.finalize_ready_now().expect("ready-now drain");
    assert!(monitor.outputed);
    let captured_after_first = monitor.captured_output.clone();

    write_to_pipe(&mut writer, b"second\n");
    monitor
        .finalize_ready_now()
        .expect("second ready-now drain");
    assert!(monitor.outputed);
    let mut expected = captured_after_first;
    expected.extend_from_slice(b"second\n");
    assert_eq!(monitor.captured_output, expected);
    drop(writer);
}

/// Terminal rendering receives the exact raw bytes (no U+FFFD
/// substitution), prefixed once with the existing `\r\n`.
#[tokio::test]
async fn output_monitor_renderer_receives_exact_raw_bytes() {
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = stdout_monitor(read);

    let payload = b"\xff\nafter\n";
    write_to_pipe(&mut writer, payload);

    let mut rendered = Vec::new();
    let mut flush = |_: &mut TerminalRenderer, bytes: &[u8]| -> Result<()> {
        rendered.extend_from_slice(bytes);
        Ok(())
    };
    monitor
        .drain_ready_now_with_budget(256 * 1024, &mut flush)
        .expect("ready-now drain");

    let mut expected = Vec::from(b"\r\n".as_slice());
    expected.extend_from_slice(payload);
    assert_eq!(rendered, expected);
    assert_eq!(monitor.captured_output.as_slice(), payload);
    drop(writer);
}

/// Raw authority vs text presentation: the same invalid input is kept
/// byte-exact in the capture while the observer shows the lossy projection.
#[tokio::test]
async fn output_monitor_observer_lossy_projection_for_invalid_utf8() {
    let observer: SharedOutputObserver = dsh_types::observed_output::ObservedOutput::shared(1024);
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = OutputMonitor::new(read, Some(observer.clone()), ObservedStream::Stdout)
        .expect("create monitor");

    let payload = b"\xff\nafter\n";
    write_to_pipe(&mut writer, payload);
    drop(writer);
    monitor.drain_to_eof().await.expect("drain to eof");

    assert_eq!(monitor.captured_output.as_slice(), payload);
    let observed = observer.lock().expect("observer lock");
    assert_eq!(observed.snapshot().stdout, String::from_utf8_lossy(payload));
    // Valid tail survives alongside the replaced prefix.
    assert!(observed.snapshot().stdout.ends_with("after\n"));
}

/// An invalid byte sequence split across drains is preserved byte-exact;
/// the observer projects once at record completion.
#[tokio::test]
async fn output_monitor_invalid_sequence_split_across_drains() {
    let observer: SharedOutputObserver = dsh_types::observed_output::ObservedOutput::shared(1024);
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = OutputMonitor::new(read, Some(observer.clone()), ObservedStream::Stdout)
        .expect("create monitor");

    // Incomplete 4-byte sequence prefix split across three drains,
    // terminated by a newline record boundary.
    write_to_pipe(&mut writer, &[0xF0]);
    monitor.drain_ready_now().expect("drain prefix byte");
    assert!(monitor.captured_output.is_empty());

    write_to_pipe(&mut writer, &[0x90, 0x80]);
    monitor.drain_ready_now().expect("drain sequence middle");
    assert!(monitor.captured_output.is_empty());

    write_to_pipe(&mut writer, b"\n");
    drop(writer);
    monitor.drain_ready_now().expect("drain record terminator");

    let expected: &[u8] = &[0xF0, 0x90, 0x80, b'\n'];
    assert_eq!(monitor.captured_output.as_slice(), expected);
    let observed = observer.lock().expect("observer lock");
    assert_eq!(
        observed.snapshot().stdout,
        String::from_utf8_lossy(expected)
    );
}

/// A no-newline binary fragment stays pending while running, then
/// publishes byte-exact at EOF with a lossy observer projection.
#[tokio::test]
async fn output_monitor_invalid_no_newline_fragment_published_at_eof() {
    let observer: SharedOutputObserver = dsh_types::observed_output::ObservedOutput::shared(1024);
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = OutputMonitor::new(read, Some(observer.clone()), ObservedStream::Stdout)
        .expect("create monitor");

    let fragment = b"A\xffB";
    write_to_pipe(&mut writer, fragment);
    monitor
        .drain_ready_now()
        .expect("running drain holds fragment");
    assert!(monitor.captured_output.is_empty());
    assert_eq!(monitor.pending_line.as_slice(), fragment);

    drop(writer);
    monitor.drain_ready_now().expect("eof publishes fragment");
    assert_eq!(monitor.captured_output.as_slice(), fragment);
    let observed = observer.lock().expect("observer lock");
    assert_eq!(
        observed.snapshot().stdout,
        String::from_utf8_lossy(fragment)
    );
}

/// A no-newline binary fragment held across the running drain publishes
/// byte-exact at ReadyNow retirement (writer still open, no EOF).
#[tokio::test]
async fn output_monitor_invalid_no_newline_fragment_published_at_retirement() {
    let observer: SharedOutputObserver = dsh_types::observed_output::ObservedOutput::shared(1024);
    let (read, mut writer) = unnamed_pipe();
    let mut monitor = OutputMonitor::new(read, Some(observer.clone()), ObservedStream::Stdout)
        .expect("create monitor");

    let fragment = b"A\xffB";
    write_to_pipe(&mut writer, fragment);
    monitor
        .drain_ready_now()
        .expect("running drain holds the fragment");
    assert!(monitor.captured_output.is_empty());

    monitor
        .finalize_ready_now()
        .expect("retirement publishes it");
    assert_eq!(monitor.captured_output.as_slice(), fragment);
    assert!(monitor.pending_line.is_empty());
    let observed = observer.lock().expect("observer lock");
    assert_eq!(
        observed.snapshot().stdout,
        String::from_utf8_lossy(fragment)
    );
    drop(writer);
}
