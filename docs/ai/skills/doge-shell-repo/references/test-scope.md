# Test Scope

- `cargo test -p dsh-builtin`: builtin, chat, MCP, runtime skill loading
- `cargo test -p dsh`: parser, repl, completion, prompt, shell behavior
- `cargo test`: cross-crate changes only
- `cargo check --workspace`: broad compile check when behavior spans many crates

Do not start with workspace-wide tests unless the change clearly crosses crate boundaries.
