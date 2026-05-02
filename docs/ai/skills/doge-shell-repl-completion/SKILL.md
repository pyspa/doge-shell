---
name: doge-shell-repl-completion
description: Use for doge-shell completion, ghost text, suggestion, skim, fuzzy, TAB, 補完, 候補, サジェスト, or ゴーストテキスト behavior. Narrows reads to repl and completion code and keeps validation inside the doge-shell package.
---

# Doge Shell REPL Completion

- Start with `rg -n "completion|suggest|ghost|skim|fuzzy|TAB" dsh/src`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for the expected entry points.
- Read [../doge-shell-repo/references/read-boundaries.md](../doge-shell-repo/references/read-boundaries.md) before opening broader files.
- Default read targets are `dsh/src/completion/`, `dsh/src/repl/completion/`, and `dsh/src/repl/input_analysis.rs`.
- Keep completion engine, suggestion prediction, and ghost text responsibilities separated unless the task explicitly asks to change their boundary.
- Validate with `cargo test -p doge-shell` unless the change clearly crosses crate boundaries.
- If package-level tests fail only from macOS sandbox child-`dsh` tracing initialization and the edit is library-scoped, rerun a focused `cargo test -p doge-shell --lib` and report the environment-dependent failure.
