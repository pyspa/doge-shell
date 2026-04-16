---
name: doge-shell-validation
description: Use for doge-shell validation planning or smallest-test selection. Chooses the narrowest cargo command from package names, task type, and crate boundaries.
---

# Doge Shell Validation

- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before selecting `cargo test -p ...`.
- Read [../doge-shell-repo/references/test-scope.md](../doge-shell-repo/references/test-scope.md) for the default validation boundaries.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) when the change maps to a known subsystem.
- Default to `cargo test -p doge-shell` for `dsh/` changes and `cargo test -p dsh-builtin` for builtin/chat changes.
- Use `cargo test` or `cargo check --workspace` only when the change clearly spans crates.
