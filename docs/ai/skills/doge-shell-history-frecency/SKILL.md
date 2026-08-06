---
name: doge-shell-history-frecency
description: Use for doge-shell history, command timing, frecency, z ranking, 履歴, コマンド計測, frecency, or ranking work.
---

# Doge Shell History Frecency

- Start with `rg -n "history|frecency|command_timing|timing|rank|score|z " dsh/src/history dsh-frecency/src dsh-builtin/src`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for history / frecency / command timing entries.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh/src/history/`, `dsh-frecency/src/`, `dsh/src/command_timing.rs`, `dsh-builtin/src/command_timing.rs`, and `dsh-builtin/src/z.rs`.
- Command history stores one row per distinct command string (UNIQUE index in `dsh/src/db.rs`), so anything walking "the previous N commands" — the Ctrl-R picker, `Alt+.` (`dsh/src/repl/last_arg.rs`) — walks distinct commands, not executions. Say so in user-facing docs rather than implying otherwise.
- `OutputHistory` is a different store and is newest-first; see [../doge-shell-repo/references/invariants.md](../doge-shell-repo/references/invariants.md).
- Validate touched packages: `cargo test -p dsh-frecency` for frecency crate changes; `cargo test -p doge-shell` for `dsh/`; `cargo test -p dsh-builtin` for builtin command changes.
