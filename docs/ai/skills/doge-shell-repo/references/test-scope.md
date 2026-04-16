# Test Scope

- `cargo test -p dsh-builtin`: builtin, chat, MCP, runtime skill loading
- `cargo test -p doge-shell`: parser, repl, completion, prompt, shell behavior
- `cargo test`: cross-crate changes only
- `cargo check --workspace`: broad compile check when behavior spans many crates

The `dsh/` directory uses the Cargo package name `doge-shell`, so prefer package names from `package-map.md` when selecting commands.

Do not start with workspace-wide tests unless the change clearly crosses crate boundaries.
