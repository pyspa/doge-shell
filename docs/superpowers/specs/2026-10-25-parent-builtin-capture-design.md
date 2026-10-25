# Parent builtin capture design

## Intent and scope

A standalone foreground builtin must finish even when its noninteractive automatic
capture exceeds pipe capacity. Preserve parent shell mutations, exact output,
observers/history, status, redirected output and isolated pipeline execution.
Arithmetic and completion changes are out of scope. Linux and macOS remain peers;
no new dependency, toolchain, child process or public configuration is needed.

## Evidence

A clean current develop build (`cargo build --locked --offline -p doge-shell
--bin dogesh`) times out at five seconds displaying a 100,000-byte alias, with no
stdout. The same 100,009 output bytes complete through a file or pipeline.
`launch_stage` waits for the parent builtin before returning its capture monitor;
the builtin blocks writing to the pipe while nobody drains it.

## Approaches and decision

- Run every builtin in a child: rejected because parent state would be lost.
- File-backed spool: avoids pipe pressure but changes storage and monitor reactor
  assumptions, delays output, and introduces disk failure behavior.
- **Selected engineering approach:** a scoped, dedicated reader thread only while
  a parent builtin writes to an automatic capture pipe. Reuse OutputMonitor's
  byte framing, renderer failure behavior and byte-budgeted direct reads.

A close-on-exec wake pipe ends the reader interval. Poll data and wake descriptors;
check wake first and return the same monitor without flushing its partial record.
This works even when the builtin blocks a single-thread Tokio runtime. After join,
the original launch/Job path still closes the capture writer, finalizes EOF,
publishes any pending fragment and records history. No ownership is duplicated.

## Lifetime and errors

A guard owns the worker and wake writer; normal finish or Drop closes wake and
joins. Spawn/setup failure closes owned descriptors before running the builtin.
Read errors are returned through finish; renderer errors remain best effort.
Cancellation/panic cannot detach a worker. Poll retries EINTR. Each drain is
byte-budgeted so a readable pipe cannot starve shutdown. Legitimate downstream
stdout backpressure remains the renderer's existing behavior.

## Acceptance

Large single/multiline builtin output, repeated calls under a low FD limit,
redirects, pipelines, parent state and smart-pipe capture all retain their
contracts. Unit tests exercise single-thread execution, partial/raw bytes,
observer exactly-once behavior and guard Drop with an open producer. No signal
handler or process-group policy changes. Independent design review supports the
reader interval; code review and Linux/macOS CI must check the final patch.
