---
name: doge-shell-process-pty
description: Use for doge-shell process, PTY, job control, raw terminal, colored output, stdout rendering, プロセス, ジョブ, raw mode, or 端末出力 bugs. Keeps reads around process and terminal boundaries.
---

# Doge Shell Process PTY

- Start with `rg -n "pty|PtyMonitor|raw mode|cfmakeraw|isatty|ANSI|stdout|job" dsh/src/process dsh/src/shell dsh/src/terminal`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for the process / PTY entry.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh/src/process/io.rs`, `dsh/src/process/job_pty.rs`, `dsh/src/process/pty.rs`, `dsh/src/shell/eval.rs`, and `dsh/src/terminal/`. Spawn-boundary work starts at `dsh/src/process/child_exec.rs` (raw post-fork child), `dsh/src/process/reexec/` (shared re-exec protocol), and `dsh/src/shell/substitution.rs` (producer ownership).
- `fork()` child では raw syscall（`setpgid`/`setsid`/`sigaction`/`dup2`/`close`/`ioctl`/`execve`/`_exit`）以外を実行しない。`tracing`・`anyhow`・確保・lock・`std::process::exit` は親側（exec-error pipe の診断整形）へ。Rust を実行する子は `posix_spawn` + fresh `dogesh` helper のみ。
- Background builtin policy は `BUILTIN_COMMAND` の `BuiltinSpec.background_mode` が authoritative。`BackgroundBuiltinMode` 型/accessor/test は `dsh-builtin/src/background.rs`。Session-bound builtin は明示拒否し、fork へ戻さない。
- Keep display-only fixes at the PTY/stdout boundary unless the task proves captured output or command execution semantics are involved.
- Cron jobs do **not** go through this path: `dsh/src/cron/exec.rs` spawns a detached `sh -c` child with stdin on `/dev/null` and its own process group, with no PTY and no `Job`. `Shell` is `!Send`, so a claimed run executes in its own `dogesh -c "cron run-job <uuid>"` child (`dsh/src/cron/run_job.rs`). See [../doge-shell-repo/references/invariants.md](../doge-shell-repo/references/invariants.md).
- A foreground child must not inherit terminal state the shell set up for itself (raw mode, the status line's scroll region). Pause it for the whole lifetime of the child, not just up to the spawn.
- `fg` resumes on the existing async foreground wait so background `OutputMonitor`s keep draining; never add a sync monitor drain duplicate.
- A job removed from `wait_jobs` for `fg` returns when still active/stopped (real observed `Stopped`, never synthesized), even on wait/SIGCONT error; completed jobs stay dropped.
- `bg` selects stopped work from the process tree, marks stopped stages running only after SIGCONT succeeds, and requeues the original active `Job` before propagating resume errors.
- Pipeline stop state distinguishes `has_stopped_process` from `is_fully_stopped`: SIGCONT decisions use the former; foreground wait/Job summary use the latter. `Job.state` is derived from the process tree, never the source of truth.
- Foreground pipeline lifecycle completion is strict: every stage in the canonical `JobProcess` tree must be `Completed`. Final-consumer success alone never drops the `Job` or kills remaining/stopped stages.
- ECHILD is a wait observation, never a synthetic ProcessState. The code that consumes an actual child status owns recording that status into the canonical process tree; later ECHILD observers must not invent an exit code.
- Execution/process changes go through the 4-layer harness: shell semantics → `dsh/tests/spec/*.toml` + `cargo test -p doge-shell --test shell_contract`; lifecycle → `dsh/src/process/job/lifecycle_property_tests.rs`; FD/PID/PGID ownership and concurrency → `dsh/tests/resource_contract.rs`. Known bugs become XFAIL contracts plus `dsh/tests/spec/xfail-allowlist.txt` (exact match); fixes must turn XFAIL into XPASS before removing the marker. Full rules: [../doge-shell-repo/references/invariants/execution.md](../doge-shell-repo/references/invariants/execution.md).
- Validate with `cargo test -p doge-shell`; use a narrower test filter only after identifying the affected module.
