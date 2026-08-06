---
name: doge-shell-prompt-terminal-ui
description: Use for doge-shell prompt, right prompt, transient prompt, terminal UI, renderer, プロンプト, 右プロンプト, transient, 端末描画, or layout bugs.
---

# Doge Shell Prompt Terminal UI

- Start with `rg -n "prompt|right prompt|transient|render|renderer|terminal|title" dsh/src/prompt dsh/src/terminal dsh/src/repl`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for the prompt / terminal UI entry.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh/src/prompt/`, `dsh/src/terminal/`, and `dsh/src/repl/`.
- Keep prompt composition changes in `dsh/src/prompt/` unless REPL rendering or terminal state proves involved.
- Read [../doge-shell-repo/references/invariants.md](../doge-shell-repo/references/invariants.md) before changing anything that draws: `print_prompt` vs `redraw_prompt`, the DECSTBM scroll region, and `print_above_prompt` each have a rule that is easy to break silently.
- The optional bottom-row status line is `dsh/src/repl/status_line.rs` (default off, `(pref-status-line t)`, `DSH_STATUS_LINE=0` forces off).
- Validate with `cargo test -p doge-shell`.
