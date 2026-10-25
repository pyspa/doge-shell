# Parent builtin capture implementation plan

> For agentic workers: use superpowers:executing-plans for this single cohesive fix.

**Goal:** Eliminate automatic-capture deadlocks without moving stateful builtins out of the parent.
**Architecture:** Temporarily move the existing OutputMonitor to an owned reader
thread; wake, join and transfer it back before ordinary Job finalization.
**Tech stack:** Rust, std threads, libc poll, existing OutputMonitor/Tokio and test harnesses.
**Spec:** `docs/superpowers/specs/2026-10-25-parent-builtin-capture-design.md`.

## Constraints and review focus

- Linux/macOS; existing MSRV/dependencies; no develop updates or unrelated fixes.
- One reader owner; original writer/redirect closure stays authoritative.
- No dependence on Tokio worker availability while a synchronous builtin runs.
- Cancellation and errors must not detach workers or retain descriptors.
- Partial/invalid UTF-8 output, observers and smart-pipe history remain exact.
- Pipeline/redirect and parent-state behavior need regression coverage.

## Task 1: Red regression

- [x] Build pristine current source and reproduce the large alias timeout; file
  and pipeline controls succeed. Preserve the build log outside the repository.
- [x] Add integration cases to `dsh/tests/noninteractive_output.rs`: large alias,
  repeated captures, redirect/pipeline controls, parent alias/cd/export state and
  previous-output reuse; use the bounded isolated process harness.
- [x] Add an execution contract to `dsh/tests/spec/simple-command.toml` for
  parent builtin capture/state. Run the large-output test and record timeout.

## Task 2: Reader lifetime

- [x] Add `io/output_monitor/concurrent.rs` with an owned guard:
  `start(OutputMonitor) -> Result<Self>` and `finish(self) -> Result<OutputMonitor>`.
  Poll capture and close-on-exec wake pipe; reuse budgeted ready-now drain, keep
  pending records for final EOF, stop on wake before reading more data.
- [x] Guard Drop closes wake and joins. Cover current-thread runtime, large raw
  output, partial observer framing, wake with producer open and repeated lifetimes.
- [x] In `job_process_launch.rs`, scope the guard to Parent builtin automatic
  capture only. Finish before returning the existing monitor to Job. Preserve
  existing error unwinding and all writer ownership.
- [x] Run the new regression and focused monitor tests until green.

## Task 3: Verification and publication

- [x] Run shell_contract, resource_contract and doge-shell package tests; fmt,
  package all-target Clippy, portability/runtime/execution authority/file-budget.
- [x] Obtain independent diff review, address findings and rerun affected checks.
- [ ] Recheck current develop/open PRs and branch diff; commit as mopemope (author
  and committer), push this feature branch and verify remote SHA.
- [ ] Create Japanese draft PR targeting develop, attach its URL, inspect exact-head
  CI to terminal state, repair introduced failures; do not merge.

## Self-review

The design preserves the parent state and single monitor/FD authority. A detached
Tokio task would not suffice on current-thread runtimes; the owned native thread
and explicit wake avoid that dependency. Stopping must not finalize pending bytes
before Job's normal EOF. Tests must assert actual output, not only exit status.
No unresolved user decision is needed within the authorized scope.
