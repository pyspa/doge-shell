# Module Map

## Crates
- `doge-shell` package (`dsh` lib / binary): shell runtime, parser, repl, completion, prompt, lisp
- `dsh-builtin`: builtin commands, AI chat tools, MCP plumbing
- `dsh-openai`: OpenAI-compatible client and config loading
- `dsh-types`: shared types
- `dsh-frecency`: frecency scoring

## Common entry points
- shell startup: `dsh/src/main.rs`, `dsh/src/lib.rs`
- parser: `dsh/src/parser/`
- completion: `dsh/src/completion/`, `dsh/src/repl/completion/`
- repl key handling: `dsh/src/repl/key_handlers/`
- builtin chat: `dsh-builtin/src/chatgpt.rs`
- builtin tools and skill loading: `dsh-builtin/src/chatgpt/tool/`, `dsh-builtin/src/chatgpt/skills.rs`
- OpenAI config and client: `dsh-openai/src/config.rs`, `dsh-openai/src/client.rs`

## Search hints
- command or builtin behavior: `rg -n "<name>" dsh-builtin dsh`
- prompt rendering: `rg -n "prompt|right prompt|transient" dsh/src/prompt dsh/src/repl`
- AI / chat / tools: `rg -n "chat|skill|tool_call|MCP" dsh-builtin/src dsh-openai/src`
- completion issue: `rg -n "completion|candidate|skim|generator" dsh/src`
