# doge-shell (dsh)

A modern, feature-rich shell written in Rust with an integrated Lisp interpreter and AI-powered command completion.

## 🐕 Overview

doge-shell (dsh) is a simple yet powerful shell that combines traditional shell capabilities with modern features like AI-assisted command completion, frecency-based history, and an embedded Lisp scripting environment.

## 💻 Supported Platforms

dsh targets **Linux** (x86_64 / aarch64) and **macOS** (Apple Silicon / Intel). Both are
first-class: the shell, its tests and its host-aware completions are expected to behave the
same on either. Windows is not supported.

Completion definitions for platform-specific tools (`systemctl`, `journalctl`, `pacman`,
`brew`, …) ship on every platform and simply produce no candidates where the tool is
absent, so a Linux-only definition never gets in the way on macOS.

## ✨ Features

### Core Shell Features

- **Interactive Command Line**: Full-featured interactive shell with readline-like functionality
- **Command Execution**: Execute external commands, built-in commands, and shell scripts
- **Background Processing**: Run commands in background with `&` and manage jobs
- **Pipes and Redirections**: Support for pipes (`|`), structured pipes (`|:`), input/output redirection (`>`, `>>`, `<`), and error redirection
- **Signal Handling**: Proper handling of signals like SIGINT, SIGQUIT, SIGTSTP
- **Subshells**: Support for command substitution and process substitution (`<(...)` only; `>(...)` not supported yet)
- **Safe Paste**: Bracketed paste support ensures pasted multi-line text is not executed immediately

### Advanced Features

- **Command Palette**: Unified interface for accessing shell commands and features with `Alt+x`
- **Frecency-based History**: Intelligent command history using frecency scoring (frequency + recency)
- **Context-Aware History**: Prioritizes commands based on the current directory or Git repository context
- **Queryable History**: Search history by text, scope, exit status, and duration with the `history` command
- **Directory Navigation**: Smart directory history and jump with `z` command
- **Directory Stack**: `pushd` / `popd` / `dirs`, plus `cd -N` to jump straight to a numbered entry
- **Path Management**: Dynamic PATH management with `add_path` command
- **Job Control**: Background job management with `jobs`, `bg`, `fg` commands
- **Aliases**: Command aliasing with `alias` command
- **Variables**: Environment variable management with `var`, `set` commands
- **Abbreviations**: Define global abbreviations with `abbr -a g git`, or command-scoped ones with `abbr --add --command git co checkout`; scoped definitions win only in that pipeline segment
- **Macro Recorder**: Record sequences of commands as reusable macros with `Alt+m`

### Completion & UI

- **Context-Aware Completion**: Intelligent tab completion for commands, files, and options
- **Skim Integration**: Fuzzy finding interface for completion using [skim](https://github.com/lotabout/skim)
- **History Search**: Interactive history search with Ctrl+R, seeded with the current input. Each candidate shows its exit status, duration, age and directory, and the scope (global / session / cwd / project), status and slow-command filters can be toggled live from inside the picker
- **Command Abbreviations**: Define and use abbreviations with `abbr` command
- **AI-Powered Completion**: OpenAI integration for intelligent command completion suggestions
- **Execution Status & Duration**: The prompt shows the exit status and elapsed time of the previous command
- **Job Notifications**: Finished background jobs are reported as `[1]+  Done  <cmd>` above the prompt without disturbing what you are typing
- **Inline Argument Explainer**: Displays real-time descriptions of command arguments and options below the prompt as you type
- **Transient Prompt**: Automatically collapses the prompt after command execution to keep the terminal clean
- **Status Line**: Optional bottom-row line with scheduled tasks, jobs, git and GitHub state (off by default)
- **Custom Key Bindings**: Rebind any key or chord from `config.lisp` with `bind`, including to Lisp functions

### 🛡️ Safety Guard

The Safety Guard protects against unintended execution of potentially destructive commands.

- **Safety Levels**:
  - `Loose`: No restrictions.
  - `Normal` (Default): Requires confirmation for common dangerous commands (`rm`, `mv`, `cp`, `dd`, `mkfs`, `format`).
  - `Strict`: Requires confirmation for **all** commands.
- **AI Tool Integration**: Automatically intercepts AI-generated commands and file modifications, requiring explicit user approval. Commands the chat agent wants to run go through the same `SafetyGuard` as commands you type, including pipeline checks such as `curl | sh`, with wrappers like `sudo` looked through so the real command is the one judged. Chat tools keep workspace-root and path-traversal protections even in loose mode; the agent can read and write within the project root and the runtime skills directory, and nowhere else.
- **Sensitive File Policy**: Chat `read_file`, `search`, `ls`, and `edit` respect `.gitignore` (nested files, `.git/info/exclude` and global excludes included), fail closed when ignore policy cannot be evaluated, hide common secret paths, and redact secret-like content in search results.
- **Lisp Configuration**: Dynamically change the safety level at any time.
  ```lisp
  (safety-level "strict") ; Enable confirmation for everything
  (safety-level "normal") ; Default safety
  (safety-level "loose")  ; Disable safety checks
  (safety-level)          ; Get current safety level
  ```
- **Environment Variable**: `SAFETY_LEVEL` reflects the current safety level (e.g., "normal", "strict").

### 🔒 Secret Management

Protection of sensitive information from history and display.

- **Automatic History Filtering**: Commands containing sensitive keywords (e.g., `API_KEY=xxx`) are automatically excluded from shell history by default.
- **History Modes**:
  - `skip` (Default): Secrets are never saved to history.
  - `redact`: Secrets are replaced with `***` before saving to history.
  - `none`: No filtering is applied.
- **Session Secrets**: Store sensitive values that are only available during the current session and never persisted to disk.
- **AI Search Redaction**: Secret-like values such as tokens, passwords, authorization headers, query tokens, and private key markers are masked before being returned from chat search results.
- **Lisp Configuration**:
  ```lisp
  (secret-add-pattern "MY_.*_KEY") ; Add custom regex pattern
  (secret-add-keyword "PRIVATE")   ; Add custom keyword
  (secret-history-mode "redact")   ; Change history filtering mode
  (secret-set "DB_PASS" "xxx")     ; Set session-limited secret
  (secret-get "DB_PASS")           ; Get session secret
  (secret-clear)                   ; Clear all session secrets
  ```

### Lisp Interpreter

- **Embedded Lisp**: Built-in Lisp interpreter for shell scripting
- **Configuration**: Shell configuration in Lisp with `~/.config/dsh/config.lisp`
- **Custom Commands**: Define custom shell commands using Lisp
- **Extensibility**: Extend shell functionality with Lisp functions

### Model Context Protocol (MCP) Integration

- **MCP Client**: Connect to external Model Context Protocol servers
- **Multiple Transport**: Support for stdio and Streamable HTTP transports; legacy SSE configs are parsed but configuration-only/deprecated
- **Dynamic Tools**: Automatic discovery of MCP server tools
- **Configuration**: MCP servers are configured in `config.lisp`

### 📊 Structured Data Pipeline

Seamlessly handle structured data (JSON, CSV, Tables) within the shell pipeline.

- **`|:` Operator**: The "Structured Pipe" operator allows you to process command output as structured data using Lisp expressions.
  ```bash
  command |: (lisp-expression)
  ```
  The command output is bound to the `$_` variable in the Lisp expression.

- **Declarative Output Schemas**: For known commands (`ps`, `ls -l`, `df`,
  `free`, `docker ps`/`images`, `git log`/`status`, `kubectl get`), `|:`
  parses the output into a typed table automatically — no hand-written
  parsing. Column types are declared in `output-schemas/*.json` (embedded;
  `~/.config/dsh/output-schemas/` overrides, meta schema in
  `command-output-schema.json`), so `%CPU` is a number and `-h` sizes are
  bytes (unsuffixed `df`/`free` columns keep their native units and say so in
  the column name, e.g. `avail_1k`, `total_kib`):
  ```bash
  # $_ is already a table with typed columns
  ps aux |: (table-where-cmp $_ "cpu" ">" 50)
  ls -l |: (table-order-by $_ "size" :desc)
  docker ps |: (table-where-contains $_ "Status" "Up")
  ```
  Where a machine-readable mode exists, the schema injects it (`docker ps`
  runs with `--format '{{json .}}'`, `kubectl get` with `-o json`) so parsing
  is exact. The raw text stays available as `$RAW`, `(output-parse "ps aux"
  text)` applies a schema explicitly, and anything without a schema — or any
  parse failure — falls back to the plain string in `$_` exactly as before.

- **Supported Formats**:
  - **JSON**: `json-parse`, `json-stringify`
  - **CSV**: `csv-parse`, `csv-stringify`
  - **Table**: Powerful table manipulation functions

- **Table Operations**:
  - **Viewing**: `table-display` (rich terminal UI), `table-head`, `table-tail`
  - **Filtering**: `table-where-eq`, `table-where-contains`, `table-where-cmp`
  - **Sorting**: `table-order-by`
  - **Transformation**: `table-select` (pick columns), `table-count`
  - **AI Integration**: `table-to-ai-context` creates an optimized context string for LLMs

- **Examples**:
  ```bash
  # View JSON as a table
  cat data.json |: (table-display (json-parse $_))

  # Filter and Sort CSV
  cat users.csv |: (table-display \
    (table-order-by \
      (table-where-cmp (csv-parse $_) "age" ">=" 18) \
      "age" :desc))

  # Convert CSV to JSON
  cat data.csv |: (json-stringify (csv-parse $_)) > data.json
  ```

### Other Features

- **Git Integration**: Commands for Git operations (`ga`, `gco`, `glog`, etc.)
- **Auto-Correction Suggestion**: Suggests similar commands when a typo is detected (e.g., "Did you mean: git ?" when typing `gti`)
- **Command Output History**: Capture command output with `|>` operator and reference it with `$OUT` variable
- **Command Timing Statistics**: Track execution time and frequency with `timing` command
- **UUID Generation**: Built-in UUID generation with `uuid` command
- **Batch Rename**: Batch file renaming with `dmv` command
- **Web Server**: Built-in static file server with `serve` command
- **Configuration Reload**: Runtime configuration reloading with `reload` command
- **Trigger Command**: Monitor file changes matching a glob pattern and automatically execute commands. Results are captured in the [output history](#command-output-history).
- **Scheduled Tasks**: Run a command every 30s/5m/1h in the background with `sched`, quietly by default and reporting only on failure or changed output
### Project Manager

Organize and switch between workspaces efficiently with the integrated Project Manager.

- **`pm add [path] [name]`**: Register a project.
- **`pm init [name]`**: Register the current project root and show onboarding status.
- **`pm status [--json]`**: Show current project registration, activation provider, mise trust/lockfile/missing tools, Dev Container detection, runtimes, and tasks.
- **`pm list`**: List registered projects (sorted by last access).
- **`pm work <name>`**: Switch to a project and trigger hooks.
- **`pm jump` / `pj`**: Interactively select and switch to a project.
- **`pm activate --provider auto|native|mise`**: Apply safe native activation and, for trusted or conservatively classified safe mise projects, overlay `mise --no-hooks env --json`. dsh never runs `mise trust`, installs tools, or executes hooks automatically.
- **`pm activate --dry-run`**: Preview `.env`, allowed `.envrc`, venv, and PATH changes with sensitive values masked before applying them.
- **Hooks**: Define `*on-project-switch-hooks*` in Lisp to automate environment setup.
  - Automatically triggered when entering a project directory (via `pm work`, `pj`, or `cd`).
  - Sets `DSH_PROJECT` environment variable to the current project name.

### Task Catalog

`task --json` exposes a stable `id/source/name/command/description/cwd` schema.
Run a task unambiguously with `task <source>:<name> -- <args>`. Static project
file parsers remain the fallback; when installed, mise, Nx, and Turbo providers
prefer their official JSON output with a bounded timeout. Results are cached by
project-marker metadata, and Nx/Turbo continue to own graph, affected, and
artifact-cache behavior.

For example, `task npm:test -- --watch` invokes `npm run test -- --watch`.
Arguments for other providers are forwarded using that runner's native syntax.

### GitHub Integration

Monitor your GitHub notifications directly from the prompt. Grouped by priority:
- `🐙`: GitHub Status (Header)
- `🔍`: Review Requested (Cyan)
- `🔔`: Mentions/Assignments (Yellow)
- `📬`: Other Notifications (Dimmed)

**Configuration**:
Set your Personal Access Token (PAT) and update interval in `config.lisp`.

```lisp
(vset "*github-pat*" "your_token_here")
(vset "*github-notify-interval*" "60") ;; seconds
```

### `gh-notify` Command

Run `gh-notify` to view notifications directly in an interactive list.
- **Select**: Use arrow keys to navigate.
- **Open**: Press `Enter` to open the notification in your browser.


## 🔧 Built-in Commands

The shell includes many built-in commands:

| Command             | Description                                                                                                                |
| ------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `exit`              | Exit the shell                                                                                                             |
| `cd`                | Change directory                                                                                                           |
| `history`           | Search and filter command history                                                                                          |
| `z`                 | Jump to frequently used directories (use `-i` or `--interactive` for selection, `-` for previous directory, `-l` for list) |
| `pushd`             | Push a directory onto the directory stack and change to it                                                                 |
| `popd`              | Pop a directory off the directory stack and change to it                                                                   |
| `dirs`              | Show the directory stack                                                                                                   |
| `jobs`              | Show background jobs                                                                                                       |
| `fg`                | Bring job to foreground                                                                                                    |
| `bg`                | Send job to background                                                                                                     |
| `lisp`              | Execute Lisp expressions                                                                                                   |
| `set`               | Set shell variables                                                                                                        |
| `var`               | Manage shell variables                                                                                                     |
| `read`              | Read input into a variable                                                                                                 |
| `abbr`              | Configure abbreviations                                                                                                    |
| `alias`             | Configure command aliases                                                                                                  |
| `export`            | Set export attribute for shell variables                                                                                   |
| `task`              | Task runner command                                                                                                        |
| `procs`             | Interactive process viewer                                                                                                 |
| `project`           | Project management (see also `pm`, `pj`)                                                                                   |
| `snippet`           | Snippet management command                                                                                                 |
| `bookmark`          | Bookmark management command                                                                                                |
| `chat_prompt`       | Set AI assistant system prompt                                                                                             |
| `chat_model`        | Set AI model                                                                                                               |
| `chat_reset`        | Forget the carried AI chat conversation and cached MCP tool list                                                           |
| `gh-notify`         | View GitHub notifications interactively                                                                                    |
| `glog`              | Git log with interactive selection                                                                                         |
| `gco`               | Git checkout with interactive branch selection                                                                             |
| `ga`                | Git add with interactive file selection                                                                                    |
| `add_path`          | Add path to PATH environment variable                                                                                      |
| `serve`             | Start a static file server                                                                                                 |
| `uuid`              | Generate UUIDs                                                                                                             |
| `dmv`               | Batch file renaming                                                                                                        |
| `reload`            | Reload shell configuration                                                                                                 |
| `timing`            | Show command execution statistics                                                                                          |
| `out`               | Display captured command output history                                                                                    |
| `blocks`            | List, inspect, rerun, and explain session command blocks                                                                   |
| `include`           | Execute a bash script and import environment variables                                                                     |
| `mcp`               | Manage MCP servers (status, connect, disconnect)                                                                           |
| `gpr`               | GitHub Pull Request checkout with interactive selection                                                                    |
| `gwt`               | Git Worktree management (add, list, remove)                                                                                |
| `pm`                | Project Manager (init, status, add, list, remove, work, jump, activate, activate --dry-run)                                |
| `pj`                | Jump to a project (alias for `pm jump`)                                                                                    |
| `help`              | Show command details and search built-in commands                                                                          |
| `comp-gen`          | Generate or audit command completion JSON (`--stdout`, `--check`, `--audit`, `--list-dynamic-providers`)                  |
| `dashboard`         | Show integrated dashboard (System, Git, GitHub)                                                                            |
| `doctor`            | Diagnose config, AI, MCP, project, runtime, skills, safety, setup, and dev validation state                                |
| `ai-commit` / `aic` | Generate commit message using AI                                                                                           |
| `tm`                | Search and retrieve past command outputs                                                                                   |
| `trigger`           | Monitor file changes and execute commands (saves output to history)                                                        |
| `sched`             | Run a command periodically in the background for this session                                                              |
| `notebook-play`     | Play a notebook file (execute code blocks interactively)                                                                   |
| `eproject`          | Open current project in Emacs                                                                                              |
| `eview`             | Pipe content to external editor                                                                                            |
| `magit`             | Open Magit status for the current directory                                                                                |
| `safe-run`          | Execute commands with deterministic and AI-powered safety analysis                                                         |
| `ai-watch`          | Explicitly watch a command with AI and save the summary to command blocks                                                  |

## 🧠 Lisp Functions

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
- `sh` - Execute shell commands in the current shell context
- `sh!` - Execute shell commands with output capture
- `setenv` - Set environment variables
- `vset` - Set shell variables
- `add_path` - Add paths to PATH
- `pref-auto-pair` - Configure automatic pairing of quotes/brackets
- `pref-auto-notify` - Configure automatic notification
- `pref-ai-explanation` - Configure AI-powered command explanations
- `pref-status-line` - Enable the bottom-row [status line](#status-line) (off by default)
- `pref-failure-hint` - Show a one-line proactive hint after a failed command (on by default; off also disables automatic AI fixes)
- `set-auto-fix-enabled` - Enable or disable AI auto-fix
- `safety-level` - Configure safety level (`loose`, `normal`, `strict`)
- `set-notify-config` - Configure notification behavior
- `allow-direnv` - Configure direnv roots
- `edit` - Open a file in the external editor

### Interactive UI Functions

- `selector` - Open an interactive fuzzy selection menu with custom prompt and items.
  - Usage: `(selector "Prompt" '("Item1" "Item2") [multi])`
  - If `multi` is true, returns a list of selected items. Default is false (single selection).

### Command Palette Integration

- `register-action` - Register a custom action in the Command Palette.
  - Usage: `(register-action "Name" "Description" "function-name")`

### Scheduled Task Functions

- `sched-add` - Register a periodic task
  - Usage: `(sched-add "<name>" "<interval>" "<command>" ["<notify-policy>"])`
- `sched-remove` / `sched-pause` / `sched-resume` - Manage a task by name or id
- `sched-list` - List the registered tasks (the `sched list` command is easier from the prompt)

See [Scheduled Tasks](#scheduled-tasks) for intervals and notify policies.

### Key Binding Functions

- `bind` - Bind a key or chord to an action or Lisp function
  - Usage: `(bind "ctrl-g" "cancel-completion")`, `(bind "ctrl-x s" "insert-snippet")`
- `unbind` - Remove a binding so the key falls back to its built-in meaning
- `list-bindings` - List the configured bindings
- `list-bind-actions` - List every action name `bind` accepts

See [Custom Key Bindings](#custom-key-bindings) for the key syntax and precedence rules.

### Hook System Functions

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
- **`DSH_NO_PTY` environment variable**: Set `DSH_NO_PTY=1` to globally disable PTY.

## 📁 Configuration

### config.lisp

Create a `~/.config/dsh/config.lisp` file to configure your shell:

> **Note**: the whole file is evaluated as a single `(begin ...)` form. If any expression fails,
> the shell rolls the environment back and **none** of the configuration takes effect, so keep an
> eye on the error printed at startup.

```lisp
;; Example configuration
(alias "ls" "ls --color=auto")
(alias "ll" "ls -alF")
(alias "la" "ls -A")
(alias "l" "ls -CF")

;; Set environment variables
(setenv "EDITOR" "vim")
(setenv "PAGER" "less")

;; Set shell variables
(vset "MY_VAR" "my_value")

;; Set abbreviations
(abbr "g" "git")
(abbr "ga" "git add")
(abbr "gc" "git commit")
(abbr "gs" "git status")
(abbr-command "git" "co" "checkout") ; only expands `co` after `git`

;; Add paths to PATH
(add_path "~/bin")
(add_path "~/.cargo/bin")

;; MCP server configuration using Lisp functions
(mcp-clear)  ; Clear any existing servers before adding new ones

;; Add MCP server with stdio transport (for local executable servers)
;; Parameters: label, command path, arguments list, environment variables list, working directory (optional), description (optional)
(mcp-add-stdio 
  "local-dev-tools"                    ; label
  "/path/to/your/mcp-server"          ; command
  '("arg1" "arg2")                    ; arguments list
  '(("ENV_VAR1" "value1") ("ENV_VAR2" "value2"))  ; environment variables list
  '()                                 ; working directory (NIL = current directory)
  "Local development tools via stdio"  ; description
)

;; Add MCP server with Streamable HTTP transport
;; Parameters: label, URL, authentication header (optional), allow stateless (optional), description (optional)
(mcp-add-http 
  "remote-http-service"               ; label
  "https://example.com/mcp"           ; URL
  '()                                 ; authentication header (NIL = no auth)
  '()                                 ; allow stateless (NIL = false)
  "Remote HTTP MCP service"           ; description
)

;; Add legacy MCP server with SSE transport
;; NOTE: legacy SSE is configuration-only/deprecated because rmcp removed the legacy SSE client transport.
;; Use mcp-add-http for Streamable HTTP MCP servers.
;; Parameters: label, URL, description (optional)
(mcp-add-sse 
  "streaming-service"                 ; label
  "https://example.com/sse"           ; URL
  "SSE-based MCP service"             ; description
)

;; Chat execute allowlist - commands the AI assistant may run WITHOUT asking.
;; Anything not listed here is not refused: it is judged by the same safety
;; guard that covers commands you type, so at the default `normal` safety level
;; an ordinary command runs after one confirmation (answer `a` to stop being
;; asked about it for the rest of the session) and a risky one is refused.
;; An entry matches by prefix, so "cargo test" also covers "cargo test -p foo".
;;
;; This list is separate from the commands you approve with `a` at your own
;; safety prompts: approving a command for yourself does not approve it for
;; the AI.
(chat-execute-clear)
;; You can add multiple commands in a single call:
(chat-execute-add "ls" "cat" "echo" "grep" "find")
;; Or add them one by one as before:
;(chat-execute-add "ls")
;(chat-execute-add "cat")
;(chat-execute-add "echo")
;(chat-execute-add "grep")
;(chat-execute-add "find")

;; Hook System - Functions that run at specific shell events
;; Define a function to use as a hook
(defun my-pre-prompt-func ()
  (print "Pre-prompt hook executed")
  ;; You can update variables, check status, etc.
)

(defun my-pre-exec-func (command)
  (print (string-append "About to execute: " command))
)

(defun my-post-exec-func (command exit-code)
  (print (string-append "Executed " command " with exit code: " (number->string exit-code)))
)

(defun my-chdir-func ()
  (print (string-append "Changed directory to: " (getenv "PWD")))
)

;; Add functions to the appropriate hook lists
;; Note: add-hook expects the base name without asterisks - it adds them internally
(add-hook 'pre-prompt-hooks 'my-pre-prompt-func)
(add-hook 'pre-exec-hooks 'my-pre-exec-func)
(add-hook 'post-exec-hooks 'my-post-exec-func)
(add-hook 'on-chdir-hooks 'my-chdir-func)
```

### tmux title integration

`dsh` updates the terminal title while a foreground command is running. To let tmux reflect that in the window name, add this to `~/.tmux.conf`:

```tmux
set -g allow-rename on
set -g automatic-rename off
```

If you also want the outer terminal emulator title to follow tmux, enable:

```tmux
set -g set-titles on
```

### MCP Configuration Details

MCP (Model Context Protocol) allows the shell to connect to external services that provide tools for AI assistants. You can configure MCP servers in your `config.lisp` file using these functions:

#### `(mcp-clear)`

Removes all currently configured MCP servers.

#### `(mcp-list-tools)`

Lists all available tools from registered MCP servers. Returns a list of tool names.

#### `(mcp-add-stdio label command args env-vars cwd description)`

Adds an MCP server that communicates via standard input/output streams.

- `label`: A unique identifier for the server
- `command`: Path to the server executable
- `args`: List of command-line arguments to pass to the server
- `env-vars`: List of (key value) pairs for environment variables
- `cwd`: Working directory for the server (or NIL for current directory)
- `description`: Optional description of the server

Example:

```lisp
(mcp-add-stdio 
  "git-tools" 
  "/usr/local/bin/git-mcp-server" 
  '("--verbose") 
  '(("GIT_AUTHOR_NAME" "Your Name")) 
  '() 
  "Git utility tools"
)
```

#### `(mcp-add-http label url auth-header allow-stateless description)`

Adds an MCP server that communicates via HTTP requests.

- `label`: A unique identifier for the server
- `url`: The HTTP endpoint for the server
- `auth-header`: Authentication header value (or NIL)
- `allow-stateless`: Whether to allow stateless operations (or NIL)
- `description`: Optional description of the server

Example:

```lisp
(mcp-add-http 
  "remote-api" 
  "https://api.example.com/mcp" 
  '("Bearer your-token-here") 
  '() 
  "Remote API server"
)
```

#### `(mcp-add-sse label url description)`

Adds a legacy MCP Server-Sent Events config entry.
This function is deprecated/configuration-only: runtime connection attempts return an error because rmcp removed the legacy SSE client transport. Use `(mcp-add-http ...)` for Streamable HTTP MCP servers.

- `label`: A unique identifier for the server
- `url`: The SSE endpoint URL
- `description`: Optional description of the server

Example:

```lisp
(mcp-add-sse 
  "events-service" 
  "https://events.example.com/stream" 
  "Real-time events service"
)
```

```

### Security & Safety

- **Execution Confirmation**: When `SafetyLevel` is set to `Normal` or `Strict`, the shell will ask for confirmation before executing any MCP tool that might have side affects.

### MCP CLI Management

You can also manage MCP servers interactively using the `mcp` command:

- `mcp status`, `mcp s`: Show connection status of all servers.
- `mcp connect <label>`, `mcp c`: Connect to a specific server.
- `mcp disconnect [label]`, `mcp d`: Disconnect from a server (or all).
- `mcp list`, `mcp l`: List registered servers.
- `mcp tools`, `mcp t`: List available tools.

## 🔧 Usage

### Basic Usage

```bash
# Start the shell interactively
dsh

# Execute a single command
dsh -c "echo 'Hello, World!'"

# Execute a Lisp script
dsh -l "(print \"Hello from Lisp!\")"
```

### Smart Pipe

Use `|` at the start of a command to pipe the output of the immediately preceding command.

```bash
# First command
echo "Hello, World!"

# Pipe the output to the next command
| tr '[:upper:]' '[:lower:]'
# Output: hello, world!
```

This works with any command (external or built-in) as the shell automatically captures the standard output.

### Command Output History

Use `out` for direct access to captured output and `tm` for interactive fuzzy search with preview.

```bash
# Show the most recent captured stdout
out

# List captured outputs with previews
out --list --limit 25

# Clear captured output history
out --clear
```

### Command Blocks

Every foreground interactive command is recorded as a session-local command block, including commands that produce no output. Blocks keep command text, cwd, exit code, duration, captured output references, and optional `ai-watch` summaries.

```bash
# List recent blocks
blocks

# Inspect a failed command
blocks list --failed
blocks show 1 --stderr

# Reuse or explain a block
blocks command 1
blocks fix 1             # deterministic, inserts/runs nothing
blocks fix 1 --ai        # explicit AI fallback
blocks explain 1

# Machine-readable session or opt-in persistent metadata
blocks list --json
blocks --scope persistent --json

# Browse them full-screen (same as Ctrl+O)
blocks tui

# Export blocks as an executable Markdown runbook
blocks export --range 1..5 -o runbook.md
blocks export --ids 3,7 --title "Deploy" --ai   # AI adds one-line step notes
notebook-play runbook.md                        # replay it step by step
```

#### Runbooks

`blocks export` turns recorded blocks into a Markdown runbook: each command is
a ```` ```sh ```` block, output excerpts are quoted (never re-executed), and
metadata such as exit codes rides in HTML comments. The result plays back with
`notebook-play`, which confirms each step before running it — record a
procedure once, then replay or share it.

Runbooks can be parameterized with the same `{{name}}` / `{{name:default}}`
markers snippets use: `notebook-play` prompts for each placeholder once (or
takes `--var name=value` for non-interactive use). Only identifier-shaped
names count as placeholders, so Go-template syntax in recorded commands
(`docker ps --format '{{json .}}'`) replays verbatim. Select blocks by display
index (`--range 1..5`, `--last 3`) or by stable id (`--ids`); steps are always
written oldest-first. `--ai` asks the configured model for a one-line
description per step, and the export still succeeds without them if the
request fails.

#### Block Browser

`Ctrl+O` opens a two-pane browser over this session's blocks: the list on one
side, the selected block's captured output on the other. Escape sequences are
stripped and `\r` progress bars are collapsed to their final state, so a
`cargo build` block reads as a handful of lines rather than thousands. Output
from full-screen programs such as `vim` is almost entirely cursor positioning,
so those blocks start folded.

- `j` / `k` / `↑` / `↓` - move in the list, or scroll when the output pane has focus
- `Tab` - switch pane · `g` / `G` - top / bottom · `Ctrl+D` / `Ctrl+U` - page the output
- `Space` - fold / unfold · `s` - cycle stdout / stderr / both · `W` - toggle wrapping
- `/` - filter by command **or output text** · `f` - failed only · `w` - `ai-watch` blocks only
- `c` / `y` - copy the command / the output
- `Enter` - put the command in the input buffer · `r` - re-run it · `d` - `cd` to where it ran
- `e` - explain it with AI · `?` - key help · `q` / `Esc` - close
- `m` - mark for export · `x` - export marked (or selected) blocks as a runbook

Copying uses the system clipboard, falling back to OSC 52 so it still works over
SSH. Blocks remain session-local by default. `blocks --scope persistent` reads
the opt-in command ledger described below; it contains metadata only unless
output storage was explicitly enabled. Filters such as `--failed` are applied
to the complete persistent ledger before `--limit` selects the newest results.

### Machine-readable output

`task --json`, `pm status --json`, `doctor --json`, `history --json`,
`blocks list --json`, and `timing --json` each write exactly one JSON value to
stdout. JSON mode and non-TTY output omit ANSI decoration and interactive
prompts; diagnostics go to stderr and failures return a non-zero status. Field
names use `snake_case`, and optional collections are emitted as empty arrays
rather than presentation text.

Focused doctor calls keep the same envelope and add section-specific `details`;
for example, `doctor safety --json` reports safety configuration while
`doctor validate --json` reports the selected validation commands.

Run as `blocks tui` there is no input buffer to fill, so `Enter` prints the
command and `r` runs it. From `Ctrl+O` and the command palette, both put it in
the buffer for you to confirm.

Many blocks legitimately have no output: it is only captured for a foreground
external command that is not redirected, not part of a pipeline, and not
PTY-proxied. The browser says so rather than showing an empty pane.

### Status Line

An optional line pinned to the bottom row, showing scheduled tasks, background jobs,
git state and GitHub notifications:

```
⏱ 3 failing 1   ⚙ 2 jobs    main ●4 ↑1   🐙 5
```

**Off by default.** Enable it in `config.lisp`:

```lisp
(pref-status-line t)
```

`DSH_STATUS_LINE=0` forces it off regardless, and it stays off on a non-terminal or a
terminal shorter than three rows.

It works by reserving a scroll region (DECSTBM) so the bottom row sits outside the
scrolling area — the prompt, command output and job notices are unaffected. The region
is released whenever something else needs the whole screen (`Ctrl+R`, `Ctrl+O`, `Alt+x`,
tab completion, `$EDITOR`, and any foreground command) and restored afterwards, and it
is always released on exit.

Everything shown is read from caches the shell already maintains, so the status line
never adds work to the prompt. It is off by default because DECSTBM support varies
between terminals.

### Scheduled Tasks

`sched` runs a command on a repeating interval in the background:

```bash
sched add 5m git fetch --all            # every 5 minutes
sched add --name prs --on change 10m gh pr list
sched add --quiet 30s 'df -h /'
sched list                              # id, interval, next run, last result
sched log prs                           # recent runs
sched rm prs
sched pause                             # stop everything, keeping the task list
sched resume
```

Intervals are `30s` / `5m` / `1h` — between 5 seconds and 24 hours. There is no cron
syntax: tasks do not outlive the shell, so wall-clock scheduling would be misleading.

**Nothing is printed on a normal run.** Output goes to the [output history](#command-output-history)
and [command blocks](#command-blocks), reachable with `out`, `blocks` and `tm`. `out` and `tm`
label the run `sched:<name> <command>` so it is distinguishable from something you typed;
the command block keeps the plain command so `blocks rerun` still works. Whether a run
interrupts you is set by `--on`:

| `--on`      | Reports                                              |
| ----------- | ---------------------------------------------------- |
| `never`     | Nothing (`--quiet` is a shorthand)                   |
| `failure`   | The command started failing, and again when it recovers |
| `change`    | The output differs from the previous run              |
| `both`      | Either of the above — **default**                    |
| `always`    | Every run                                            |

Failures report on the *transition*, not on every run: a task failing every 30 seconds
says so once, then again when it recovers.

Notices appear above the prompt without disturbing what you are typing, and a desktop
notification follows if `pref-auto-notify` is on.

**Commands run under `sh -c`**, in the directory where you registered them, with stdin
on `/dev/null` and in their own process group. So they cannot steal the terminal or be
hit by `Ctrl+C` at the prompt — but shell aliases, abbreviations, builtins and Lisp
functions are **not** available inside them. Write out the full command, or wrap it in a
script.

A run that overruns its own interval is skipped rather than stacked, at most two tasks
run at once, and each run is killed after its timeout (60s by default, capped to the
interval).

Tasks are session-scoped. To make them permanent, put `sched-add` in `config.lisp` —
`sched list --lisp` prints exactly those lines for the tasks you have now:

```lisp
(sched-add "fetch" "5m" "git fetch --all")
(sched-add "prs" "10m" "gh pr list" "change")
```

### Snippets

`Alt+;` opens the snippet list (managed with the `snippet` command) and inserts the
chosen command at the cursor. Typing a character dismisses the list instead of
selecting — this inserts a whole command, so an accidental keystroke should not
commit to one.

A snippet body can carry `{{name}}` or `{{name:default}}` markers. On insertion each
marker is replaced by its default (nothing, when there is no default) and the cursor
lands on the first one. `Alt+n` and `Alt+p` cycle through the remaining stops.

```bash
snippet add deploy 'kubectl rollout restart deploy/{{name}} -n {{ns:default}}'
```

Inserting that gives `kubectl rollout restart deploy/ -n default` with the cursor
after `deploy/`; type the deployment name, then `Alt+n` moves onto `default`. The stops
follow your edits, so filling one in does not throw the others off.

They are dropped once you leave the line — running the command, recalling history,
undo/redo, or opening the editor or a picker.

Note that `Tab` is left alone — it stays completion, so paths can still be completed
while filling in a placeholder.

### Insert Last Argument

`Alt+.` inserts the last argument of the previous command, and each repeat replaces
it with the argument from the command before that. Quoting is preserved, so
`"hello world"` comes back as one argument.

Because history stores one row per distinct command string, the walk is over
*distinct commands*, not over individual executions — the same caveat that applies to
[History Search](#history-search). Adjacent duplicates are skipped so repeated
presses always show something new.

### Directory Stack

`pushd` / `popd` / `dirs` work the way they do in bash: slot 0 of the stack is always
the current directory, so a plain `cd` replaces the top without disturbing what is
underneath.

```bash
dirs -v          # 0  ~/src/doge-shell
pushd /tmp       # /tmp ~/src/doge-shell
pushd ~/notes    # ~/notes /tmp ~/src/doge-shell
pushd            # swap the top two: /tmp ~/notes ~/src/doge-shell
popd             # back to ~/notes
dirs -c          # clear everything but the current directory
```

`dirs` takes `-v` (numbered, one per line), `-p` (one per line), `-l` (full paths
instead of `~`), and `-c` (clear).

Any entry can be addressed by its `dirs -v` number:

```bash
cd -2            # jump to entry 2, rotating it to the top
pushd +2         # same thing
popd +1          # drop entry 1 without moving
```

**One difference from bash**: `+N` and `-N` mean the same thing here — "entry N as
printed by `dirs -v`". bash counts `-N` from the other end, which is a daily papercut
for no real gain. A bare `cd -` still means `$OLDPWD`, unchanged.

`pushd` and `popd` go through the same code path as `cd`, so directory frecency (`z`),
`$OLDPWD` and `*on-chdir-hooks*` all keep working.

### Import History

Import command history from other shells:

```bash
# Import from fish shell
dsh import fish

# Import from bash with custom path

dsh import bash --path /path/to/bash_history
```

### `history` Command

Search command history with text and metadata filters.

```bash
# Search by text
history cargo

# Show recent failures
history --status failure

# Show slow commands in the current project with metadata
history --scope project --slow 1000 --verbose

# Query append-only ledger events by provenance
history --author human --json
```

`--scope` accepts `global`, `session`, `cwd`, and `project`. Use `--limit` to control result count, or `--query` if you prefer an explicit flag instead of the positional search text.

The aggregate history remains the default. To opt in to the append-only command
ledger, add `(pref-command-ledger "metadata")` to `config.lisp`; use `"output"`
only when captured output should be inspected, redacted, and retained (64 KiB
per event, at most 10,000 events, 90-day retention). The default `"off"` and
`"metadata"` modes do not inspect or store command output. External agents can
record provenance with `history record --json '<event>'`. Optional Atuin
dual-write is enabled with `DSH_ATUIN_DUAL_WRITE=1`; failures run off the prompt
path and never block a command.

### `doctor` Command

Inspect the current shell setup and project context.

```bash
# Run all diagnostics
doctor

# Focus on one area
doctor ai
doctor project
doctor skills
doctor setup
doctor fix
doctor validate

# Show command help
doctor --help
```

`doctor` reports on configuration files, AI settings, MCP connection counters, project marker files, common developer runtimes found in `PATH`, performance/cache state, runtime Skill drift, setup readiness, and focused validation commands for changed files. `doctor fix` creates safe missing setup files and directories such as `config.lisp`, runtime skills, and completion directories.

### `help` Command

Search built-in commands and show command-specific usage.

```bash
help
help doctor
help project
help --search ai
```

### Project Onboarding

Register the current project and inspect what dsh can activate or run.

```bash
pm init
pm status
pm status --json
pm activate --provider auto --dry-run
pm activate --provider auto
task --json
task cargo:test -- --nocapture
```

### `include` Command

Source a bash script and import its environment variables into the current shell session.
Useful for loading `.env` files or setup scripts.

```bash
include setup.sh
```

### Key Bindings

- `Tab` - Context-aware completion
- `Ctrl+R` - Interactive history search using the current input as the query (see [History Search](#history-search))
- `Ctrl+C` - Cancel current command (press twice to exit shell)
- `Ctrl+D` - End of input (exit) on an empty line, delete the character under the cursor otherwise
- `Ctrl+Z` - Suspend the running command; on an empty prompt, resume the most recently stopped job
- `Ctrl+O` - Open the [Block Browser](#block-browser) over this session's commands and their output
- `Ctrl+L` - Clear screen
- `Home` / `End` - Move to the beginning / end of the line (same as `Ctrl+A` / `Ctrl+E`)
- `Delete` - Delete the character under the cursor
- `Ctrl+K` - Delete from cursor to end of line
- `Ctrl+U` - Delete from cursor to beginning of line
- `Ctrl+W` - Delete word backward
- `Ctrl+Y` - Yank back the text removed by the last `Ctrl+K` / `Ctrl+U` / `Ctrl+W`
- `Ctrl+_` - Undo the last edit (undo steps break at word boundaries)
- `Alt+/` - Redo
- `Alt+.` (or `Alt+_`) - Insert the last argument of the previous command; press again to walk further back
- `Alt+;` - Pick a snippet and insert it at the cursor
- `Alt+n` / `Alt+p` - Jump to the next / previous `{{placeholder}}` of an inserted snippet
- `Alt+x` - Open Command Palette
- `Esc` (double press) - Toggle `sudo` prefix for the current command
- `Ctrl+x Ctrl+e` - Edit current input in external editor (`$VISUAL` or `$EDITOR`)

All of the above are defaults and can be changed — see [Custom Key Bindings](#custom-key-bindings).

- `Alt+Enter` - Execute command in background
- `Alt+s` - Force AI suggestion
- `Alt+[` / `Alt+]` - Rotate through suggestions
- `Alt+w` - Wrap the current input with `ai-watch --`
- `Alt+m` - Open Macro Recorder

### Custom Key Bindings

Keys can be rebound from `config.lisp`:

```lisp
(bind "ctrl-g" "cancel-completion")
(bind "alt-." "insert-last-argument")
(bind "ctrl-x s" "insert-snippet")   ; multi-key chords work
(bind "f5" "trigger-completion")
(unbind "ctrl-x ctrl-e")             ; back to the built-in meaning
```

`list-bindings` shows what is configured and `list-bind-actions` every name `bind`
accepts. Both are Lisp functions, so from the prompt they go through `lisp`:

```bash
lisp "(print (list-bindings))"
lisp "(print (list-bind-actions))"
```

Key syntax is `[modifier-]*key`, with `-` or `+` as the separator. Modifiers are
`ctrl`/`c`, `alt`/`m`/`meta`, `shift`/`s` and `super`. Key names are a single character
(`a`, `.`, `/`) or one of `enter`, `tab`, `shift-tab`, `space`, `esc`, `backspace`,
`delete`, `insert`, `home`, `end`, `pageup`, `pagedown`, `up`, `down`, `left`, `right`,
`f1`–`f12`. Chords are strokes separated by spaces.

**A bound key wins unconditionally.** Several built-in bindings are context-sensitive —
`Right` accepts a suggestion when one is showing, `Esc` toggles `sudo` only when no
completion is open, `Ctrl+D` is EOF only on an empty line. Rebinding such a key replaces
all of that with the single action you named. This is the same rule as zsh's `bindkey`
and fish's `bind`.

If the name is not a built-in action it is taken as a Lisp function, called with the
current input and cursor position. Returning a string inserts it at the cursor;
returning anything else leaves the buffer alone:

```lisp
(fn insert-date (input cursor)
  (sh! "date +%Y-%m-%d"))
(bind "ctrl-t" "insert-date")
```

The call is synchronous, so a slow function blocks the prompt — the same caveat as
Command Palette Lisp actions.

`Ctrl+x Ctrl+e` (edit in `$EDITOR`) is itself a default binding and can be unbound or
moved. A chord that goes nowhere (`Ctrl+x q`) drops the prefix and lets the second key
do its normal job, so a stray `Ctrl+x` never costs you a keystroke.

### History Search

`Ctrl+R` opens a full-screen history picker, seeded with whatever you have already typed.
Each row shows the exit status, duration, age and directory of the command, so a failed or
slow run is recognisable at a glance. Narrower terminals drop the optional columns, keeping
the status glyph and the command itself.

The filters that the `history` command exposes as flags are available as live toggles:

- `Ctrl+R` - cycle scope: `global` → `session` → `cwd` → `project`
- `Ctrl+S` - cycle status: `any` → `failure` → `success`
- `Ctrl+T` - show only commands that took a second or more
- `Up` / `Down`, `Ctrl+P` / `Ctrl+N`, `PageUp` / `PageDown`, `Home` / `End` - move the selection
- `Ctrl+U` - clear the query
- `Enter` - put the command in the input buffer (it is never executed for you)
- `Esc`, `Ctrl+C`, `Ctrl+G` - cancel and leave the input untouched

The status line reports the active filters and how many entries match, e.g.
`scope:cwd  status:failure  slow:off  (12/5000, last run)`.

**`last run`**: history is stored one row per distinct command string, carrying only the
metadata of its most recent execution. `scope:cwd` therefore means "commands whose *last*
run was in this directory", not "every command ever run here", and the exit status and
duration shown are those of that last run.

Set `DSH_HISTORY_PICKER=skim` to fall back to the previous skim-based interface.

### Fish Completion Fallback

TAB completion calls `fish -c 'complete -C ...'` automatically when `fish` is available in `PATH`. Leave `DSH_COMPLETION_FISH_FALLBACK` unset for auto mode, set it to `1`, `true`, `yes`, or `on` to force it on, or set it to `0`, `false`, `no`, or `off` to disable it. This fallback runs with a timeout, is cached with other external completion results, and is merged below built-in JSON and project-aware dynamic candidates.

Built-in dynamic providers also complete live resource identifiers for supported tools such as `gh`, `glab`, `argocd`, cloud CLIs, Vault, Nomad, Terraform/OpenTofu, rclone, and restic. They invoke only read-only listing commands, run off the prompt thread, use a five-second command timeout and a 30-second cache, and quietly return no candidates when a CLI is missing, unauthenticated, or unavailable. Provider diagnostics report cache age and errors but never include tokens, passwords, or fetched secret contents. Additional providers cover 1Password (`op item get/edit/delete`), Vagrant installed boxes (`vagrant box remove/repackage`, `vagrant init`), and `.envrc` files plus their containing directories discovered from the working directory chain for `direnv allow/deny/edit`.

Linux operations use the same dynamic completion path for systemd units and machines, journal identifiers, network namespaces and links, firewall profiles, storage resources, processes, kernel modules, audit keys, SELinux modules and booleans, wireless devices, device subsystems, and the login shells listed in `/etc/shells`. Arch Linux definitions cover `yay`, `paru`, `paccache`, `pacdiff`, `pactree`, `pacman-conf`, `pacman-key`, `pkgctl`, repository database tools, devtools chroot commands, `namcap`, and `snapper`. `yay` and `paru` deliberately reuse only the local pacman databases: sync operations complete repository packages, removal operations complete installed packages, and completion never performs an AUR network search. Snapper configurations and snapshots, pacman repositories, and mkinitcpio presets are discovered with read-only local probes. A `systemctl.unit` provider scope narrows candidates to a single unit type, so `systemd-run --slice` offers slices rather than every unit. Candidates reflect the current host while read-only probes remain cached and run outside the prompt thread.

Developer toolchains are covered as well: `rustup` components and targets, crates installed with `cargo install`, `cargo` test and bench targets, `cargo nextest`, `cargo watch`, `sccache`, `bacon`, Jujutsu (`jj`), PDM, Pipenv, Pyright, Biome, golangci-lint, GoReleaser, Delve, Meson, watchexec, and ghq, in addition to the existing language and editor tools. Dynamic candidates include Jujutsu bookmarks, revisions, and workspaces; Meson targets; golangci-lint linters; ghq repositories; and scripts or jobs declared by bacon, PDM, and Pipenv. Definitions that live in project files — `bacon.toml` jobs, `pyproject.toml` PDM scripts, `Pipfile` scripts, `noxfile.py` sessions, `tox` environments, `hatch` environments, `pre-commit` hook ids, and `just` recipes and `make` targets through the shared project task provider — are parsed directly, so completion never imports or evaluates project code. Local CLI probes use a one-second cache and two-second error backoff; missing tools, permission failures, timeouts, and malformed output safely produce no dynamic candidates.

## 💻 Command Palette

Access all shell capabilities through a unified fuzzy-search interface, similar to VS Code's Command Palette.

- **Trigger**: Press `Alt+x` to open.
- **Features**:
  - Run internal commands and setup helpers (Doctor Setup/Fix, Project Init/Status, Output History, etc.)
  - Access AI features (Explain, Fix, etc.)
  - Run `safe-run` against the current input or generate completion JSON for the current command name
  - Execute Git operations
  - Extensible via the `Action` trait and Lisp `register-action`

## Command Suggestions

When a command is not found, dsh can suggest close command names. If the current directory exposes tasks through the built-in task runner, task suggestions may also appear as `task <name>` candidates.

After a failed command, `Alt+f` first uses the local deterministic Quick Fix
engine (command/Git typos, missing upstream, occupied port, local execute bit,
and missing project runtime). It only falls back to the configured AI fixer
when no deterministic candidate exists. Suggestions only replace the input;
they are never executed automatically. The same engine is available for old
failures through `blocks fix <N> [--json] [--ai]`. Port fixes require an
explicit `:PORT` or `port PORT` diagnostic so version, PID, and errno numbers do
not become accidental kill suggestions.

Inside VS Code, dsh emits each OSC 633 command marker once in A/B/E/C/D order,
plus Cwd and HasRichCommandDetection properties. Prompt redraws do not duplicate
the B marker. Other terminals keep the existing OSC 133 and OSC 7 integration.

## 📼 Macro Recorder

Easily record and replay sequences of shell commands without writing code manually.

1. **Trigger**: Press `Alt+m` to open the Macro Recorder.
2. **Select Commands**:
   - The interface shows your recent command history.
   - Use `Up`/`Down` arrows to navigate.
   - Press `Tab` to select/deselect multiple commands.
   - Press `Enter` to confirm selection.
3. **Name Macro**: Enter a name for your new macro (e.g., `daily-setup`).
4. **Use**: The macro is immediately available as a custom command.
   ```bash
   daily-setup
   ```
5. **Persistence**: The macro is saved as a Lisp function in your `config.lisp` file, so it persists across sessions.
   ```lisp
   (defun daily-setup ()
     (sh "cd ~/project")
     (sh "git pull")
     (sh "cargo build")
   )
   ```

## 🤖 AI Integration

The shell includes AI-powered command completion using OpenAI. To use this feature:

1. Set your OpenAI API key in the environment:

   ```bash
   export AI_CHAT_API_KEY="your-api-key-here"
   ```

   Optional settings:

   | Variable | Default | Purpose |
   | --- | --- | --- |
   | `AI_CHAT_MODEL` | `gpt-5-mini` | Model used for chat and AI actions |
   | `AI_SUMMARY_MODEL` | chat model | Model used to summarize a long conversation |
   | `AI_CHAT_BASE_URL` | `https://api.openai.com/v1/` | OpenAI-compatible endpoint |
   | `AI_CHAT_ALLOW_INSECURE_HTTP` | off | Allow an `http://` base URL (local models) |
   | `AI_CHAT_TIMEOUT_SECS` | `180` | Total per-request timeout |
   | `AI_CHAT_SESSION_TTL_SECS` | `1800` | How long consecutive `!` turns share a conversation; `0` disables it |
   | `AI_CHAT_CONTEXT_TOKEN_BUDGET` | `100000` | Prompt tokens before the conversation is summarized |
   | `AI_CHAT_TURN_TOKEN_BUDGET` | unset | Stop one `!` turn once it has spent this many tokens |
   | `AI_CHAT_EXECUTE_ALLOWLIST` | unset | Extra entries for the `execute` tool allowlist, merged with `config.lisp` and the JSON config |
   | `AI_MESSAGE_LANG` | unset | Language for AI responses |

   Transient failures (429, 5xx, timeouts) are retried with backoff, honouring
   `Retry-After`.

2. The shell will automatically provide command suggestions when available.

3. Use `!` prefix to chat with the AI directly:

   ```bash
   !explain how to use the grep command
   ```

4. **Auto-Fix (Error Recovery) (`Alt+f`)**:
   If a command fails, press `Alt+f` to have AI suggest a fix for the last failed command.

   ```bash
   # Type a wrong command:
   git stats
   # Command fails...
   
   # Press Alt+f, command input becomes:
   git status
   ```

   **Proactive failure hints** (on by default): when a command fails, dsh
   automatically shows the deterministic quick-fix as ghost text with a short
   reason next to the prompt — accept it with `Tab` or `Alt+f`. No AI request
   is sent unless `set-auto-fix-enabled` is on. Interrupted commands
   (`Ctrl-C`), "no match" exits from `grep`/`diff`-style commands, and the
   same failure repeating stay quiet; for a pipeline the exit status belongs
   to its last segment, so `cat f | grep x` finding nothing stays quiet too.
   `(pref-failure-hint nil)` in `config.lisp` turns off the whole automatic
   path, automatic AI fixes included — `Alt+f` and `Alt+d` keep working on
   demand.

5. **Smart Git Commit (`Alt+c`)**:
   Stage your changes, then press `Alt+c` to invoke the `aic` command, which analyzes the diff and generates a conventional commit message.

   ```bash
   git add .
   # Press Alt+c, command input becomes:
   aic
   # Press Enter to generate message
   ```

6. **Error Diagnosis (`Alt+d`)**:
    When a command fails, press `Alt+d` to have AI diagnose the error and suggest fixes.

    ```bash
    gti status  # Typo - command fails
    # Press Alt+d, AI analyzes the error and suggests:
    # "The command 'gti' was not found. Did you mean 'git'?"
    ```

7. **Safe Run (`safe-run`)**:
    Execute commands with deterministic prechecks and AI-powered safety analysis. Useful for auditing potential risky commands or inspecting output before piping.

    ```bash
    # Analyze and execute a command
    safe-run rm -rf tmp/

    # Inspect content before piping (e.g., curl | sh)
    safe-run curl https://example.com/install.sh | sh
    ```

    - **Static Precheck**: Obvious dangerous patterns such as remote-script execution are flagged before AI analysis.
    - **Analysis**: AI checks for destructive operations or malicious patterns.
    - **Content Inspection**: For pipe operations, you can inspect the captured output (preview shown on stderr) before allowing it to pass to the next command.
    - **Confirmation**: Required for execution.

8. **AI Watch (`ai-watch`)**:
    Explicitly run a command through the normal shell execution path, then ask AI to summarize the result and save it to `blocks`.

    ```bash
    ai-watch -- cargo test -p doge-shell
    ai-watch --goal "server ready を検出" -- npm run dev
    ```

    Press `Alt+w` to wrap the current input as `ai-watch -- <current input>` without executing it.

9. **Agent tools**:
    Inside `!` chat the assistant can call `shell_history`, `shell_context`, `search`,
    `ls`, `read_file`, `str_replace`, `edit` and `execute`.

    - `shell_history` shows what you recently ran, with the working directory, the exit
      code and both output streams. Ask "why did that fail" and the assistant reads the
      failure instead of running the command again to reproduce it.
    - `shell_context` reports the project root, its runtimes, the build/test tasks it
      defines, and your aliases - so the assistant looks the test command up rather than
      guessing it.
    - `read_file` returns a line-numbered window and is paged: the assistant continues
      with `offset` instead of losing everything past the first few KB.
    - `search` matches a substring by default, or a regular expression with
      `regex: true`; `ignore_case` and a `glob` file filter are available, and binary
      files are skipped.
    - `str_replace` changes part of a file by exact match. `edit` still exists for
      creating a file or replacing one in full.
    - `execute` runs a shell command - pipes, redirection and `&&` included - and kills
      it after `timeout_ms` (120s by default), so a build or a dev server cannot wedge
      the shell. Command substitution (`$(...)`, backticks, `<(...)`) and subshells are
      refused, because evaluating them is what a safety check must not do.
    - The command is checked by the same `SafetyGuard` that covers what you type, and
      wrappers are looked through, so `sudo rm -rf ...` is judged as `rm`. At the
      default `normal` safety level that means the destructive commands the guard knows
      about ask first and everything else runs; set `(safety-level 'strict)` to be asked
      about every command the allowlist does not already cover. Anything that writes to
      a file by redirection asks as well.
    - An allowlisted command skips the question entirely - that is what putting it on
      the list means - so keep `(chat-execute-add ...)` to commands you are content for
      the AI to run unattended. Entries match by token prefix (`cargo test` covers
      `cargo test -p foo`); an "always" answer given at a prompt matches only that exact
      command line.
    - Tools can read and write within the project root and the runtime skills directory.
      `.gitignore` is honoured the way git honours it, nested files included.
    - Long tool output is truncated in the middle, so the end of a build or test log -
      where the error is - still reaches the model.
    - `edit`, `str_replace` and skill scripts always ask for confirmation.

10. **Conversation continuity**:
    Consecutive `!` turns continue the same conversation, so follow-up questions work and
    the assistant does not re-explore the project every time. It restarts when the
    directory changes, when the model/prompt/language changes, after
    `AI_CHAT_SESSION_TTL_SECS` (default 1800), or on `chat_reset`.

11. **Token usage**:
    Each `!` turn prints what it cost (`tokens: N req / in X (cached Y) / out Z`), and
    `doctor ai` shows the session total.

12. **AI Response Language**:
    Configure the language for AI chat responses.

    ```lisp
    (vset "AI_MESSAGE_LANG" "Japanese")
    ```

    Or via environment variable:
    ```bash
    export AI_MESSAGE_LANG="Japanese"
    ```

13. **Runtime Skills**:
   The chat runtime can load local skills from `~/.config/dsh/skills/`. This repository keeps canonical sample skills under `docs/ai/skills/`.

   ```bash
   scripts/install-runtime-skills.sh dsh
   ```

   Keep each skill summary in the YAML frontmatter `description`, and move long details into `references/` so runtime prompts stay compact.

For maintainers, concise AI/Skill authoring notes live in `docs/ai/README.md`.


## 📁 Project Structure

- `dsh/` - Main shell executable and core implementation
- `dsh-builtin/` - Built-in commands
- `dsh-frecency/` - Frecency-based history management
- `dsh-types/` - Shared data structures
- `dsh-openai/` - OpenAI integration
- `completions/` - Canonical command completion definitions (embedded into the binary)

## 🤝 Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Make your changes
4. Add tests if applicable
5. Run the smallest relevant validation:
   - `cargo test -p dsh-builtin` for built-in command, chat, MCP, doctor, task, or runtime Skill loading changes
   - `cargo test -p doge-shell` for parser, REPL, completion, prompt, or shell behavior changes
   - `cargo test -p dsh-openai`, `cargo test -p dsh-types`, or `cargo test -p dsh-frecency` for those crates
   - `scripts/check-ai-guidance.sh` after changing `AGENTS.md`, `docs/ai/`, or runtime Skill installer guidance
   - `doctor validate` can suggest the focused commands from your current `git status`
6. Keep runtime Skill copies current when Skill guidance changes:
   `scripts/install-runtime-skills.sh --status --target codex --profile codex-core`
7. Commit your changes (`git commit -m 'Add amazing feature'`)
8. Push to the branch (`git push origin feature/amazing-feature`)
9. Open a Pull Request

## 📄 License

This project is licensed under the MIT/Apache-2.0 license - see the [LICENSE](LICENSE) file for details.

## 🙏 Acknowledgments

- Built with [Rust](https://www.rust-lang.org/)
- Uses [skim](https://github.com/lotabout/skim) for fuzzy finding
- Inspired by modern shells like Fish and Zsh
- Includes an embedded Lisp interpreter for extensibility
- Integrates with Model Context Protocol (MCP) for AI-assisted tool access
