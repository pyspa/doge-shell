---
name: doge-shell-repl-completion
description: Use for doge-shell completion, ghost text, suggestion, skim, fuzzy, TAB, 補完, 候補, サジェスト, or ゴーストテキスト behavior. Narrows reads to repl and completion code and keeps validation inside the doge-shell package.
---

# Doge Shell REPL Completion

- Start with `rg -n "completion|suggest|ghost|skim|fuzzy|TAB" dsh/src`.
- 補完エンジンではなく `completions/*.json` の定義や dynamic provider を足す作業なら [../doge-shell-completion-spec/SKILL.md](../doge-shell-completion-spec/SKILL.md) に切り替える。
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for the expected entry points.
- Read [../doge-shell-repo/references/read-boundaries.md](../doge-shell-repo/references/read-boundaries.md) before opening broader files.
- Default read targets are `dsh/src/completion/`, `dsh/src/repl/completion/`, and `dsh/src/repl/input_analysis.rs`.
- Keep completion engine, suggestion prediction, and ghost text responsibilities separated unless the task explicitly asks to change their boundary.
- Register each dynamic provider exactly once through `dsh/src/completion/dynamic/registry.rs`; route normal and cached-only collection through the same family collector with `CachePolicy` instead of adding a second dispatch match.
- Key dispatch is a separate concern: user bindings resolve in `dsh/src/repl/keybind/` before `determine_key_action`, and the insert keys (`Alt+.`, snippet, `{{placeholder}}` stops) are in `dsh/src/repl/key_handlers/input_shortcuts.rs`. Read [../doge-shell-repo/references/invariants.md](../doge-shell-repo/references/invariants.md) before changing how a key is consumed.
- The inline grid draws at the bottom of the screen, so anything that reuses it must pause the status line first (`StatusLinePause`).
- Validate with `cargo test -p doge-shell` unless the change clearly crosses crate boundaries.
- If package-level tests fail only from macOS sandbox child-`dsh` tracing initialization and the edit is library-scoped, rerun a focused `cargo test -p doge-shell --lib` and report the environment-dependent failure.
