---
name: doge-shell-process-pty
description: Use for doge-shell PTY allocation, raw terminal mode, terminal input proxy, stdout/stderr or ANSI rendering, foreground terminal ownership, and stopped FullProxy resume.
---

# Doge Shell Process PTY

- Start with `rg -n "pty|PtyMonitor|raw mode|cfmakeraw|isatty|ANSI|stdout|input proxy" dsh/src/process dsh/src/terminal`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for the process / PTY entry.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh/src/process/io.rs`, `dsh/src/process/job_pty.rs`, `dsh/src/process/pty.rs`, and `dsh/src/terminal/`. Spawn-boundary work starts at `dsh/src/process/child_exec.rs` (raw post-fork child).
- `fork()` child では raw syscall（`setpgid`/`setsid`/`sigaction`/`dup2`/`close`/`ioctl`/`execve`/`_exit`）以外を実行しない。`tracing`・`anyhow`・確保・lock・`std::process::exit` は親側（exec-error pipe の診断整形）へ。Rust を実行する子は `posix_spawn` + fresh `dogesh` helper のみ。
- Background builtin policy は `BUILTIN_COMMAND` の `BuiltinSpec.background_mode` が authoritative。`BackgroundBuiltinMode` 型/accessor/test は `dsh-builtin/src/background.rs`。Session-bound builtin は明示拒否し、fork へ戻さない。
- Keep display-only fixes at the PTY/stdout boundary unless the task proves captured output or command execution semantics are involved.
- Cron jobs do **not** go through this path: `dsh/src/cron/exec.rs` spawns a detached `sh -c` child with stdin on `/dev/null` and its own process group, with no PTY and no `Job`. `Shell` is `!Send`, so a claimed run executes in its own `dogesh -c "cron run-job <uuid>"` child (`dsh/src/cron/run_job.rs`). See [../doge-shell-repo/references/invariants.md](../doge-shell-repo/references/invariants.md).
- A foreground child must not inherit terminal state the shell set up for itself (raw mode, the status line's scroll region). Pause it for the whole lifetime of the child, not just up to the spawn.
- A stopped FullProxy job keeps PTY/output ownership but no terminal input proxy (`pty`/`pty_mode`/`pty_output_task` stay, `pty_input_task` is `None`). `fg` resume recreates exactly one input proxy on the existing PTY session.
- Terminal raw mode is scoped to each active FullProxy foreground interval (`ForegroundPtyRawModeGuard`, reused for resume; no independent mechanism).
- `OutputMonitor`/`PtyMonitor` captured-output authority is raw bytes; only `SharedOutputObserver`/`OutputHistory` take a lossy UTF-8 text projection.
- For lifecycle/status/ownership semantics beyond PTY ownership (wait, `wait -n`/`-p`, `$!`, `fg`/`bg` lifecycle, pipeline final status, ECHILD, process substitution ownership, no-command pipelines, reexec), switch to [doge-shell-execution-semantics](../doge-shell-execution-semantics/SKILL.md).
- Validate with `cargo test -p doge-shell`; use a narrower test filter only after identifying the affected module.
