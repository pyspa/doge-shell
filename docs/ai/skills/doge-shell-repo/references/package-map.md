# Package Map

- `dsh/` -> Cargo package `doge-shell`
  - lib crate name: `doge_shell`（`dsh/Cargo.toml` に `[lib]` は無く、package 名 `doge-shell` からの既定。`use doge_shell::lib_main;` が `dsh/src/main.rs` の実例）
  - binary target: `dsh`
  - 最小検証: `cargo test -p doge-shell`
- `dsh-builtin/` -> Cargo package `dsh-builtin`
  - builtin, chat, MCP, runtime skill loading
  - 最小検証: `cargo test -p dsh-builtin`
- `dsh-openai/` -> Cargo package `dsh-openai`
  - OpenAI-compatible client, config loading
  - 最小検証: `cargo test -p dsh-openai`
- `dsh-types/` -> Cargo package `dsh-types`
  - shared types
  - 最小検証: `cargo test -p dsh-types`
- `dsh-frecency/` -> Cargo package `dsh-frecency`
  - frecency scoring
  - 最小検証: `cargo test -p dsh-frecency`

When a directory name and package name differ, prefer the Cargo package name in `cargo test -p ...`.
