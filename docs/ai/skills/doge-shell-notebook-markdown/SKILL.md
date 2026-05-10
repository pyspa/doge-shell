---
name: doge-shell-notebook-markdown
description: Use for doge-shell notebook, markdown rendering, output history, tm/out display, notebook_play, or terminal markdown work.
---

# Doge Shell Notebook Markdown

- Start with `rg -n "notebook|markdown|output history|out|tm|notebook_play|render" dsh-builtin/src dsh-types/src`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for notebook / markdown entries.
- Read [references/checklist.md](references/checklist.md) before changing rendered output or notebook block parsing.
- Keep display formatting in `dsh-builtin/src/markdown.rs`, `dsh-builtin/src/out.rs`, or `dsh-builtin/src/tm.rs` unless shared data shapes prove involved.
- Validate with `cargo test -p dsh-builtin`; add `cargo test -p dsh-types` only when notebook or output-history types change.
