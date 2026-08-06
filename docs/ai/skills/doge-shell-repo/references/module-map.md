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
- repl key handling: `dsh/src/repl/key_handlers/`, `dsh/src/repl/handler.rs`, `dsh/src/repl/key_action.rs`
- user key bindings: `dsh/src/repl/keybind/`, `dsh/src/lisp/keybind.rs`
- input insert keys (`Alt+.`, snippet, placeholders): `dsh/src/repl/last_arg.rs`, `dsh/src/repl/placeholder.rs`, `dsh/src/repl/key_handlers/input_shortcuts.rs`
- status line: `dsh/src/repl/status_line.rs`
- scheduled tasks: `dsh/src/scheduler/`, `dsh-builtin/src/sched.rs`, `dsh-types/src/schedule.rs`, `dsh/src/lisp/sched.rs`
- directory stack: `dsh-builtin/src/dirstack.rs`, `dsh-builtin/src/cd.rs`
- process / PTY / jobs: `dsh/src/process/`, `dsh/src/shell/eval.rs`
- environment and activation: `dsh/src/environment/`, `dsh/src/direnv.rs`
- history and timing: `dsh/src/history/`, `dsh-frecency/src/`, `dsh/src/command_timing.rs`
- Lisp and config startup: `dsh/src/lisp/`, `dsh/src/lib.rs`, `dsh/src/main.rs`
- command palette and AI actions: `dsh/src/command_palette/`, `dsh/src/ai_features/`, `dsh/src/argument_explainer.rs`
- builtin commands: command-specific files under `dsh-builtin/src/`, `dsh/src/proxy/builtin/`
- builtin chat: `dsh-builtin/src/chatgpt.rs`
- builtin tools and skill loading: `dsh-builtin/src/chatgpt/tool/`, `dsh-builtin/src/chatgpt/skills.rs`
- builtin serve / MCP: `dsh-builtin/src/serve/`, `dsh-builtin/src/mcp.rs`, `dsh-types/src/mcp.rs`
- OpenAI config and client: `dsh-openai/src/config.rs`, `dsh-openai/src/client.rs`

## Search hints
- command or builtin behavior: `rg -n "<name>" dsh-builtin dsh`
- prompt rendering: `rg -n "prompt|right prompt|transient" dsh/src/prompt dsh/src/repl`
- AI / chat / tools: `rg -n "chat|skill|tool_call|MCP" dsh-builtin/src dsh-openai/src`
- completion issue: `rg -n "completion|candidate|skim|generator" dsh/src`
- key binding / chord: `rg -n "keybind|Chord|KeyStroke|Resolved|BoundAction|pending_chord" dsh/src/repl dsh/src/lisp`
- scheduled task: `rg -n "sched|Scheduler|IntervalSpec|NotifyPolicy" dsh/src/scheduler dsh-builtin/src dsh-types/src`
- directory stack: `rg -n "dir_stack|pushd|popd|changepwd" dsh/src dsh-builtin/src`
- status line: `rg -n "status_line|DECSTBM|StatusLinePause" dsh/src/repl`
- PTY / process display: `rg -n "pty|PtyMonitor|raw mode|isatty|ANSI|job" dsh/src/process dsh/src/shell dsh/src/terminal`
- project environment: `rg -n "direnv|environment|activation|project" dsh/src dsh-builtin/src`
- Lisp/config issue: `rg -n "lisp|config\\.lisp|stdlib|include|reload" dsh/src/lisp dsh/src dsh-builtin/src`
- builtin command issue: `rg -n "<command>|builtin|help|project|export|task|snippet" dsh-builtin/src dsh/src/proxy/builtin`
