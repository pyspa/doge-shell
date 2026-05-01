---
name: doge-shell-repl-completion
description: Use for doge-shell completion, ghost text, suggestion, skim, fuzzy, TAB, 補完, 候補, サジェスト, or ゴーストテキスト behavior. Narrows reads to repl and completion code and keeps validation inside the doge-shell package.
---

# Doge Shell REPL Completion

- Start with `rg -n "completion|suggest|ghost|skim|fuzzy|TAB" dsh/src`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for the expected entry points.
- Read [../doge-shell-repo/references/read-boundaries.md](../doge-shell-repo/references/read-boundaries.md) before opening broader files.
- Default read targets are `dsh/src/completion/`, `dsh/src/repl/completion/`, and `dsh/src/repl/input_analysis.rs`.
- Validate with `cargo test -p doge-shell` unless the change clearly crosses crate boundaries.
