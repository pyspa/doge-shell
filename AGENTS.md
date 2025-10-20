# doge-shell Agent Handbook

## Project Snapshot
- Modern Rust shell with AI-assisted completion, Lisp interpreter, frecency history, and Model Context Protocol (MCP) client support.
- Primary binary crate lives in `dsh/`; auxiliary crates (`dsh-builtin/`, `dsh-frecency/`, `dsh-types/`, `dsh-openai/`) provide built-ins, ranking, shared types, and OpenAI integration.
- Completion assets are JSON definitions in `completions/`; repo-wide formatting config sits in `rustfmt.toml`.
- README highlights built-in commands (`chat`, `gco`, `abbr`, etc.), key bindings, and configuration expectations—consult it when documenting user-facing behavior.

## Rust Toolchain & Standards
- Workspace targets Rust Edition 2024; ensure `rustup update stable` so the latest stable compiler and standard library features are available.
- Follow `rustfmt` using the repository configuration; run `cargo fmt` before committing.
- Prefer idiomatic 2024 syntax: `use` path inlining, `if let`/`let else`, `async fn`, and pattern matching enhancements where they clarify the code.
- Keep dependencies current with workspace versions (see root `Cargo.toml`) and avoid pinning older language features unless required for compatibility.

## Build, Test, and Run
- `cargo build` for quick feedback; `cargo build --release` for optimized binaries.
- Launch the shell via `cargo run -p dsh -- [args]`; use `--help`, `-c`, or `-l` options per README examples.
- Execute tests with `cargo test` or crate-scoped invocations like `cargo test -p dsh`. Add `-- --nocapture` when inspecting interactive output.
- Lint with `cargo clippy --all-targets --all-features` and keep CI-friendly durations in mind.

## Structure & Module Guidance
- Keep new subsystems organized under `dsh/src/` alongside peers (`completion/`, `process/`, `lisp/`).
- Shared data structures belong in `dsh-types/`; reusable logic for built-ins in `dsh-builtin/`; AI-specific functionality in `dsh-openai/`.
- Match README’s feature descriptions when extending functionality (e.g., new MCP transports, job control improvements, additional built-ins).

## Testing Expectations
- Co-locate unit tests with implementation files; use integration-style tests when exercising async pipelines or shell commands.
- Name tests descriptively (`handles_pipeline_abort`, `resumes_background_job`) and cover regressions with focused cases.
- For async scenarios rely on Tokio utilities already in the workspace; avoid lengthy sleeps that slow the suite.

## Contribution Workflow
- Commit messages follow `<type>(<scope>): <subject>` in imperative mood, under 72 characters.
- Summaries in PRs should call out user-visible changes, configuration updates, and testing evidence (screenshots or logs for UX-affecting work).
- Keep branches rebased and document new Lisp forms, config keys, or CLI flags in README/AGENTS as appropriate.

## Configuration & Secrets
- AI features require `AI_CHAT_API_KEY` at runtime; never commit secrets.
- User configuration defaults to `~/.config/dsh/config.lisp`; document new special forms or environment variables clearly.
- When expanding MCP support, describe required `config.lisp` or `config.toml` entries mirroring README guidance.
