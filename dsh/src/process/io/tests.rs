use super::PtyMonitor;
use nix::unistd::pipe;
use std::io::{ErrorKind, Write as _};
use std::os::fd::{FromRawFd, IntoRawFd};
use std::time::Duration;

#[tokio::test]
async fn pty_monitor_drains_to_eof_after_stdout_failure() {
    let (read_fd, write_fd) = pipe().expect("pipe");
    let mut monitor = PtyMonitor::new(read_fd.into_raw_fd(), None).expect("create monitor");
    let mut writer = unsafe { std::fs::File::from_raw_fd(write_fd.into_raw_fd()) };
    let expected = vec![b'x'; 8192];
    writer.write_all(&expected).expect("write output");
    drop(writer);

    let mut flush_calls = 0;
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        monitor.process_output_with(|_| {
            flush_calls += 1;
            Err(std::io::Error::new(
                ErrorKind::BrokenPipe,
                "test stdout failure",
            ))
        }),
    )
    .await
    .expect("monitor did not drain to EOF");

    let err = result.expect_err("stdout failure should propagate after draining");
    assert!(
        err.to_string()
            .contains("PtyMonitor: failed to flush stdout")
    );
    assert_eq!(flush_calls, 1);
    assert_eq!(monitor.captured_output, expected);
}

// Each display callback is a read acknowledgement: write the next chunk only
// after the previous read, making split UTF-8 deterministic without sleeps.
async fn observe_chunks(chunks: &[&[u8]], real_pty: bool, fail_display: bool) {
    use dsh_types::observed_output::ObservedOutput;
    let (read_fd, write_fd) = if real_pty {
        let pty = nix::pty::openpty(None, None).expect("scratch PTY");
        (pty.master, pty.slave)
    } else {
        pipe().expect("pipe")
    };
    let observer = ObservedOutput::shared(4096);
    let mut monitor =
        PtyMonitor::new(read_fd.into_raw_fd(), Some(observer.clone())).expect("create monitor");
    let mut writer = Some(std::fs::File::from(write_fd));
    writer.as_mut().unwrap().write_all(chunks[0]).unwrap();
    let expected = chunks.concat();
    let expected_text = String::from_utf8_lossy(&expected).into_owned();
    let (writer_tx, writer_rx) = tokio::sync::oneshot::channel();
    let mut writer_tx = Some(writer_tx);
    let close_after_observation = async {
        if fail_display {
            // Display callbacks stop after failure. Keep the slave open until
            // observer text acknowledges the complete final sentinel instead.
            let writer = writer_rx.await.expect("writer from failed display");
            while observer.lock().unwrap().snapshot().stdout != expected_text {
                tokio::task::yield_now().await;
            }
            drop(writer);
        }
    };
    let mut next = 1;
    let mut displayed = Vec::new();
    let mut reads = 0;
    let monitor_output = monitor.process_output_with(|bytes| {
        if !bytes.is_empty() {
            reads += 1;
        }
        displayed.extend_from_slice(bytes);
        if next < chunks.len() {
            writer.as_mut().unwrap().write_all(chunks[next]).unwrap();
            next += 1;
        } else {
            // The final chunk has now been read. Closing immediately after
            // writing it can discard unread slave output on macOS.
            drop(writer.take());
        }
        if fail_display {
            writer_tx
                .take()
                .unwrap()
                .send(writer.take().unwrap())
                .unwrap();
            Err(std::io::Error::new(
                ErrorKind::BrokenPipe,
                "test stdout failure",
            ))
        } else {
            Ok(())
        }
    });
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(monitor_output, close_after_observation)
    })
    .await
    .expect("observer acknowledgement and drain to EOF");
    assert_eq!(monitor.captured_output, expected, "raw capture");
    if fail_display {
        assert_eq!(reads, 1, "display remains disabled after its first failure");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("failed to flush stdout")
        );
    } else {
        result.unwrap();
        assert_eq!(reads, chunks.len(), "separate actual reads");
        assert_eq!(displayed, expected, "raw display");
    }
    assert_eq!(
        observer.lock().unwrap().snapshot().stdout,
        String::from_utf8_lossy(&expected)
    );
}

#[tokio::test]
async fn pty_observer_preserves_split_utf8_actual_reads() {
    for real_pty in [false, true] {
        observe_chunks(
            &[
                b"\xe6",
                b"\x97",
                b"\xa5\xe6\x9c",
                b"\xac\xf0",
                b"\x9f\x98",
                b"\x80",
            ],
            real_pty,
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn pty_observer_invalid_and_incomplete_eof_actual_reads() {
    for real_pty in [false, true] {
        observe_chunks(&[b"\xff\xe6", b"\x97X\xf0", b"\x9f\x98"], real_pty, false).await;
    }
}

#[tokio::test]
async fn pty_observer_updates_after_display_failure() {
    for real_pty in [false, true] {
        observe_chunks(&[b"\xe6", b"\x97\xa5\xf0\x9f\x98\x80done"], real_pty, true).await;
    }
}

#[tokio::test]
async fn pty_observer_finalizes_incomplete_utf8_at_eof_after_display_failure() {
    use dsh_types::observed_output::ObservedOutput;

    let (read_fd, write_fd) = pipe().expect("pipe");
    let observer = ObservedOutput::shared(4096);
    let mut monitor =
        PtyMonitor::new(read_fd.into_raw_fd(), Some(observer.clone())).expect("create monitor");
    let mut writer = Some(std::fs::File::from(write_fd));
    writer.as_mut().unwrap().write_all(b"\xe6").unwrap();
    let mut flush_calls = 0;
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        monitor.process_output_with(|_| {
            flush_calls += 1;
            // The first read is acknowledged before the display fails. Pipes
            // retain these later bytes across close, including the pending tail
            // that cannot become observer text until EOF.
            let mut writer = writer.take().expect("only one display callback");
            writer.write_all(b"\x97\xa5\xf0\x9f\x98\x80\xe6").unwrap();
            drop(writer);
            Err(std::io::Error::new(
                ErrorKind::BrokenPipe,
                "test stdout failure",
            ))
        }),
    )
    .await
    .expect("drain pending UTF-8 to EOF after display failure");

    let err = result.expect_err("display error must propagate after drain");
    assert!(err.to_string().contains("failed to flush stdout"));
    assert_eq!(
        err.downcast_ref::<std::io::Error>().unwrap().kind(),
        ErrorKind::BrokenPipe
    );
    assert_eq!(flush_calls, 1);
    assert_eq!(monitor.captured_output, b"\xe6\x97\xa5\xf0\x9f\x98\x80\xe6");
    assert_eq!(observer.lock().unwrap().snapshot().stdout, "日😀\u{fffd}");
}
