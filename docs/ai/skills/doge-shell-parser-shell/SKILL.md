---
name: doge-shell-parser-shell
description: Use for doge-shell parser, AST, redirect, pipe, brace, expansion, パーサ, リダイレクト, パイプ, ブレース, or 展開 work. Narrows reads to parser code and keeps validation inside the doge-shell package.
---

# Doge Shell Parser Shell

- Start with `rg -n "parser|ast|redirect|pipe|brace|expand" dsh/src/parser dsh/src`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for parser entry points.
- Read [../doge-shell-repo/references/module-map.md](../doge-shell-repo/references/module-map.md) only if ownership is unclear outside `dsh/src/parser/`.
- Default read target is `dsh/src/parser/`.
- Validate with `cargo test -p doge-shell` unless the behavior crosses crates.
