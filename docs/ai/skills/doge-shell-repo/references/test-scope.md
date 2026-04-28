# Test Scope

- `cargo test -p dsh-builtin`: builtin, chat, MCP, runtime skill loading
- `cargo test -p doge-shell`: parser, repl, completion, prompt, shell behavior
- `cargo test -p dsh-openai`: OpenAI-compatible client or config loading
- `cargo test -p dsh-types`: shared type changes, especially MCP/project/output data shapes
- `cargo test -p dsh-frecency`: frecency scoring or store changes
- `cargo test`: cross-crate changes only
- `cargo check --workspace`: broad compile check when behavior spans many crates
- `scripts/check-ai-guidance.sh`: AGENTS, docs/ai, Skill, or runtime skill installer guidance changes

The `dsh/` directory uses the Cargo package name `doge-shell`, so prefer package names from `package-map.md` when selecting commands.

Do not start with workspace-wide tests unless the change clearly crosses crate boundaries.

Never use `cargo test -p dsh`; the `dsh/` directory is the `doge-shell` package.
