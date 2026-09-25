---
name: doge-shell-parser-shell
description: Use for doge-shell parser, AST, redirect, pipe, brace, expansion, パーサ, リダイレクト, パイプ, ブレース, or 展開 work. Narrows reads to parser code and keeps validation inside the doge-shell package.
---

# Doge Shell Parser Shell

- Start with `rg -n "parser|ast|redirect|pipe|brace|expand" dsh/src/parser dsh/src`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for parser entry points.
- Read [../doge-shell-repo/references/module-map.md](../doge-shell-repo/references/module-map.md) only if ownership is unclear outside `dsh/src/parser/`.
- Default read target is `dsh/src/parser/`.
- Shell planning (`dsh/src/shell/plan.rs`, `dsh/src/shell/parse/`) is side-effect-free and performs no runtime word expansion. Execution planning preserves word structure (`PlannedWord` / `WordPart`); alias rewriting is syntax-time only (`parser::rewrite_aliases`, static `argv0` spans). Variable, tilde, brace/glob and substitution expansion happen only when a selected job is materialized (`dsh/src/shell/word_expand.rs`, via `dsh/src/shell/materialize.rs` and `dsh/src/shell/process_substitution/`). Every substitution body and the final materialized argv go through `SafetyGuard` via `dsh/src/shell/authorize.rs`.
- Execution planning is strict: any non-whitespace unparsed tail is a syntax error and no prefix is executed. `Rule::commands` remains intentionally tolerant for REPL highlighting and completion. Strict execution validates raw input before alias rewriting and validates alias-rewritten input again.
- Validate with `cargo test -p doge-shell` unless the behavior crosses crates.
