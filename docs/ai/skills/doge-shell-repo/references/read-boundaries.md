# Read Boundaries

- Start with `rg --files` or `rg -n`; do not open `README.md` or broad directories first.
- Open `README.md` only when the task depends on user-facing behavior, config examples, installation guidance, or public docs updates.
- Read `module-map.md` only when crate ownership is unclear after targeted `rg`.
- Read `package-map.md` before choosing `cargo test -p ...` when the directory name may differ from the package name.
- Run `cargo test` for the whole workspace only when the change clearly crosses crate boundaries.
- Prefer `cargo check --workspace` over `cargo test` when you only need a broad compile confirmation.
- For investigation or review tasks, avoid editing and avoid broad validation until the likely files are narrowed down.
