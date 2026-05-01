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
- Validate with `cargo test -p doge-shell`; use a narrower test filter only after identifying the affected module.
