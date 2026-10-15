# 🧠 Lisp Functions

[← README](../README.md)

The embedded Lisp interpreter includes many built-in functions:

### Core Functions

- `print` - Print a value
- `is_null`, `is_number`, `is_symbol`, `is_boolean`, `is_procedure`, `is_pair`, `is_table` - Type checking
- `car`, `cdr`, `cons`, `list`, `nth`, `sort`, `reverse` - List operations
- `map`, `filter` - Higher-order functions
- `length`, `range` - List utilities
- `hash`, `hash_get`, `hash_set` - Hash map functions
- `+`, `-`, `*`, `/`, `truncate` - Arithmetic operations
- `not`, `==`, `!=`, `<`, `<=`, `>`, `>=` - Comparison operations
- `eval`, `apply` - Meta functions

### Shell Integration Functions

- `alias` - Set command aliases from Lisp
- `abbr` - Set abbreviations from Lisp
- `command` - Execute external commands and capture output
- `command-result` - Execute literal argv and return separate status/stdout/stderr data
- `sh` - Execute shell commands in the current shell context
- `sh!` - Execute shell commands with output capture
- `setenv` - Set environment variables
- `vset` - Set shell variables
- `add_path` - Add paths to PATH
- `pref-auto-pair` - Configure automatic pairing of quotes/brackets
- `pref-auto-notify` - Configure automatic notification
- `pref-ai-explanation` - Configure AI-powered command explanations
- `pref-status-line` - Enable the bottom-row [status line](usage.md#status-line) (off by default)
- `pref-failure-hint` - Show a one-line proactive hint after a failed command (on by default; off also disables automatic AI fixes)
- `pref-diagnose-hint` - When a failed command has no deterministic quick fix, point at `Alt-f`/`Alt-d` (off by default; costs no AI request, and only shown when AI is configured)
- `set-auto-fix-enabled` - Enable or disable AI auto-fix
- `safety-level` - Configure safety level (`loose`, `normal`, `strict`)
- `set-notify-config` - Configure notification behavior
- `allow-direnv` - Configure direnv roots
- `edit` - Open a file in the external editor

### Structured Command Results

`command-result` takes an executable string followed by literal argument strings;
it does not parse shell source, expand globs, or apply aliases. It returns a hash
with string keys: `exit-code` (integer, or `nil` on signal termination), `signal`
(integer signal number, or `nil` on normal exit), `stdout`, and `stderr`.
Both output strings retain trailing whitespace; invalid UTF-8 bytes become the
replacement character. Child stdin is closed, just as with `command`.

```lisp
(let ((result (command-result "grep" "needle" "input.txt")))
  (print (hash_get result "exit-code"))
  (print (hash_get result "stdout"))
  (print (hash_get result "stderr")))
```

Nonzero child exits and signal termination are result data. Invalid arguments,
SafetyGuard confirmation requirements, executable lookup failures, and spawn
errors remain Lisp errors. Execution uses the logical PATH, exported child
environment and current working directory through the same runtime snapshot as
`command`, and shares its synchronous capture/wait owner. This adds no timeout,
background-job API or separate cancellation controller. `command` still returns
trimmed stdout (or stderr when stdout is empty); `sh` and `sh!` retain their
existing shell-source contracts.

### Table / Structured Data Functions

`json-parse`, `json-stringify`, `csv-parse`, `csv-stringify`, `output-parse`, and the full `table-*` family (`table-select`, `table-head`, `table-tail`, `table-count`, `table-distinct`, `table-rename`, `table-where-eq`, `table-where-ne`, `table-where-contains`, `table-where-cmp`, `table-order-by`, `table-group-by`, `table-count-by`, `table-sum`, `table-avg`, `table-min`, `table-max`, `table-display`, `table-to-ai-context`) are the Lisp side of the `|:` structured pipe — see [Structured Data Pipeline](../README.md#-structured-data-pipeline) for usage, the DSL shorthand, and `output-gen`.

### Interactive UI Functions

- `selector` - Open an interactive fuzzy selection menu with custom prompt and items.
  - Usage: `(selector "Prompt" '("Item1" "Item2") [multi])`
  - If `multi` is true, returns a list of selected items. Default is false (single selection).

### Command Palette Integration

- `register-action` - Register a custom action in the Command Palette.
  - Usage: `(register-action "Name" "Description" "function-name")`

### Cron Functions

- `cron-add` - Register (or replace) a cron job by name
  - Usage: `(cron-add "<name>" "<schedule>" "<command>" ["<notify-policy>"])`
  - `<schedule>` accepts an interval (`5m`), a five-field cron expression, or an `@daily`-style macro.
  - Calling this again with the same name **replaces** the job rather than erroring or duplicating it — safe to leave in `config.lisp`, which runs on every launch.
- `cron-remove` / `cron-pause` / `cron-resume` - Manage a job by name or id
- `cron-list` - List the registered jobs (the `cron list` command is easier from the prompt)
- `sched-add` / `sched-remove` / `sched-pause` / `sched-resume` / `sched-list` - **Deprecated**, kept for one release as aliases for the `cron-*` functions above (same arguments and behavior). Each prints a warning to stderr only when stderr is a TTY; migrate to `cron-*`.

See [Cron Jobs](usage.md#cron-jobs) for schedule syntax and notify policies.

### Key Binding Functions

- `bind` - Bind a key or chord to an action or Lisp function
  - Usage: `(bind "ctrl-g" "cancel-completion")`, `(bind "ctrl-x s" "insert-snippet")`
- `unbind` - Remove a binding so the key falls back to its built-in meaning
- `list-bindings` - List the configured bindings
- `list-bind-actions` - List every action name `bind` accepts

See [Custom Key Bindings](usage.md#custom-key-bindings) for the key syntax and precedence rules.

### Hook System Functions

These are the shell's Lisp hooks, which fire around ordinary command execution. The AI
assistant has a separate mechanism - see [AI chat hooks](ai-integration.md#-ai-integration).

- `add-hook` - Add a function to a hook list
- `bound?` - Check if a symbol is bound
- `*pre-prompt-hooks*` - Hook list for functions to run before prompt is displayed
- `*pre-exec-hooks*` - Hook list for functions to run before command execution
- `*post-exec-hooks*` - Hook list for functions to run after command execution
- `*on-chdir-hooks*` - Hook list for functions to run after changing directory
- `*command-not-found-hooks*` - Hook list for functions to run when a command is not found (receives command name)
- `*completion-hooks*` - Hook list for functions to run when TAB completion is triggered (receives input and cursor position)
- `*input-timeout-hooks*` - Hook list for functions to run periodically when idle (every 1 second)

### MCP Management Functions

- `mcp-clear` - Clear all MCP servers
- `mcp-add-stdio` - Add an MCP server with stdio transport
- `mcp-add-http` - Add an MCP server with Streamable HTTP transport
- `mcp-add-sse` - Add a legacy SSE MCP server config (deprecated/configuration-only; use `mcp-add-http` for Streamable HTTP)
- `mcp-list` - List registered MCP servers
- `mcp-status` - Show connection status of all MCP servers
- `mcp-connect` - Connect to a specific MCP server
- `mcp-disconnect` - Disconnect from a specific MCP server
- `mcp-disconnect-all` - Disconnect from all MCP servers
- `mcp-list-tools` - List all available MCP tools
- `chat-execute-clear` - Clear execute tool allowlist
- `chat-execute-add` - Add command(s) to execute tool allowlist (accepts multiple commands)

### Suggestion Settings Functions

- `set-suggestion-mode` - Set suggestion mode (`ghost` or `off`)
- `set-suggestion-ai-enabled` - Enable/disable AI-powered suggestions

### Secret Management Functions

- `secret-add-pattern` - Add a regex pattern for secret detection
- `secret-add-keyword` - Add a keyword for secret detection
- `secret-list-patterns` - List registered secret patterns
- `secret-history-mode` - Set or get history filtering mode (`skip`, `redact`, `none`)
- `secret-set` - Set a session-only secret
- `secret-get` - Get a session-only secret
- `secret-clear` - Clear all session secrets

### PTY Control

Some interactive commands may require disabling the built-in PTY. You have two options:

- **`nopty` prefix**: Use `nopty <command>` to run a single command without PTY.
  ```bash
  nopty trizen -S google-chrome
  ```
- **`DOGESH_NO_PTY` environment variable**: Set `DOGESH_NO_PTY=1` to globally disable PTY.

