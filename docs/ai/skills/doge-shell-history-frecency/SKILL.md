---
name: doge-shell-history-frecency
description: Use for doge-shell history, command timing, frecency, z ranking, 履歴, コマンド計測, frecency, or ranking work.
---

# Doge Shell History Frecency

- Start with `rg -n "history|frecency|command_timing|timing|rank|score|z " dsh/src/history dsh-frecency/src dsh-builtin/src`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for history / frecency / command timing entries.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh/src/history/`, `dsh-frecency/src/`, `dsh/src/command_timing.rs`, `dsh-builtin/src/command_timing.rs`, and `dsh-builtin/src/z.rs`.
- Validate touched packages: `cargo test -p dsh-frecency` for frecency crate changes; `cargo test -p doge-shell` for `dsh/`; `cargo test -p dsh-builtin` for builtin command changes.
