---
name: doge-shell-validation
description: Use for doge-shell validation planning, smallest-test selection, 検証, 最小テスト, or cargo test selection. Chooses the narrowest cargo command from package names, task type, and crate boundaries.
---

# Doge Shell Validation

- Use [../doge-shell-repo/references/test-scope.md](../doge-shell-repo/references/test-scope.md) as the validation authority, including iteration versus completion criteria.
- Use [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) only when the package name is unknown.
- Start with the affected module or integration target; widen when the call path crosses module/crate boundaries or focused checks cannot prove the contract. Check that the intended tests actually ran.
- Never run `cargo test -p dsh`; the `dsh/` directory is the Cargo package `doge-shell`.
- If subprocess tests are blocked by the environment, report the blocked validation separately. A passing library test does not prove process/PTY behavior.
- For `AGENTS.md`, `docs/ai/`, or runtime skill installer guidance, run `scripts/check-ai-guidance.sh`, `scripts/install-runtime-skills.sh --list`, and focused installer `--dry-run` / `--check-installed` checks instead of Rust tests. `--status` is informational only.
- Use `cargo test` or `cargo check --workspace` only when the change clearly spans crates.
- Add `scripts/check-portability.py` when the change touches a `target_os` arm, an OS-specific source (`/proc`, `/etc/passwd`, `sysctl`), an absolute command path in a test, or `.cargo/config.toml`. `cargo clippy` only ever sees the host's arm.
- Run `scripts/check-project-consistency.py` for workspace manifest, `README.md`, or `LICENSE` changes.
- Run `./scripts/check.sh` only at the end of a staged design change or before release; it integrates fmt, guidance, project metadata, ShellProxy capability coverage, portability, file-budget, Clippy, workspace tests, and diff checks.
