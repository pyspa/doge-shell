---
name: doge-shell-repo
description: Use when working in the doge-shell repository. Helps locate the right crate or module quickly, avoid unnecessary README reads, and choose the smallest effective Rust validation command.
---

# Doge Shell Repo

- Start with `rg --files` or `rg -n`; do not open broad files first.
- Read [references/module-map.md](references/module-map.md) when ownership is unclear.
- Read [references/test-scope.md](references/test-scope.md) before choosing cargo commands.
- Open `README.md` only for user-facing docs, config examples, or feature behavior that is described there.
- Prefer the smallest test or check that proves the change.
