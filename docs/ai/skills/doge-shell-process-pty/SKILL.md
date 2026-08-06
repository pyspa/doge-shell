---
name: doge-shell-process-pty
description: Use for doge-shell process, PTY, job control, raw terminal, colored output, stdout rendering, プロセス, ジョブ, raw mode, or 端末出力 bugs. Keeps reads around process and terminal boundaries.
---

# Doge Shell Process PTY

- Start with `rg -n "pty|PtyMonitor|raw mode|cfmakeraw|isatty|ANSI|stdout|job" dsh/src/process dsh/src/shell dsh/src/terminal`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for the process / PTY entry.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh/src/process/io.rs`, `dsh/src/process/job_pty.rs`, `dsh/src/process/pty.rs`, `dsh/src/shell/eval.rs`, and `dsh/src/terminal/`.
- Keep display-only fixes at the PTY/stdout boundary unless the task proves captured output or command execution semantics are involved.
- Scheduled tasks do **not** go through this path: `dsh/src/scheduler/exec.rs` spawns a detached `sh -c` child with stdin on `/dev/null` and its own process group, with no PTY and no `Job`. `Shell` is `!Send`, so a spawned task cannot use `eval_str` — see [../doge-shell-repo/references/invariants.md](../doge-shell-repo/references/invariants.md).
- A foreground child must not inherit terminal state the shell set up for itself (raw mode, the status line's scroll region). Pause it for the whole lifetime of the child, not just up to the spawn.
- Validate with `cargo test -p doge-shell`; use a narrower test filter only after identifying the affected module.
