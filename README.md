# doge-shell (dogesh)

A modern, feature-rich shell written in Rust with an integrated Lisp interpreter and AI-powered command completion.

## 🐕 Overview

doge-shell (dogesh) is a simple yet powerful shell that combines traditional shell capabilities with modern features like AI-assisted command completion, frecency-based history, and an embedded Lisp scripting environment.

## 💻 Supported Platforms

dogesh targets **Linux** (x86_64 / aarch64) and **macOS 13 or newer** (Apple Silicon / Intel). Both are
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
- **Subshells**: Support for command substitution and process substitution (`<(...)` and `>(...)`)
- **Arithmetic Expansion**: POSIX arithmetic expansion `$((...))` with integer, variable, assignment, and conditional operators
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
- **AI-Powered Completion**: OpenAI-compatible integration for inline suggestions, error diagnosis and commit messages. Off by default — an API key alone sends nothing; see [AI Integration](#-ai-integration)
- **Execution Status & Duration**: The prompt shows the exit status and elapsed time of the previous command
- **Job Notifications**: Finished background jobs are reported as `[1]+  Done  <cmd>` above the prompt without disturbing what you are typing
- **Inline Argument Explainer**: Displays real-time descriptions of command arguments and options below the prompt as you type
- **Transient Prompt**: Automatically collapses the prompt after command execution to keep the terminal clean
- **Status Line**: Optional bottom-row line with cron jobs, background jobs, git and GitHub state (off by default)
- **Custom Key Bindings**: Rebind any key or chord from `config.lisp` with `bind`, including to Lisp functions

### 🛡️ Safety Guard

The Safety Guard protects against unintended execution of potentially destructive commands.

- **Safety Levels**:
  - `Loose`: Commands, MCP tool calls and sensitive-file reads run unasked. File
    writes are the exception - see below.
  - `Normal` (Default): Requires confirmation for destructive commands the guard
    recognises - `rm` with a recursive/force combination, `dd`, `mkfs`, `format`,
    disk and power commands, `chmod -R`/`chown -R`, `cp -rf`, `curl`/`wget` used
    to send data, remote-script pipelines such as `curl | sh`, and reads of
    sensitive files. `mv` and package-manager installs are not questioned;
    use `Strict` if you want to be asked about everything.
  - `Strict`: Requires confirmation for **all** commands.
- **Foreground `!` file writes always confirm**: the chat agent's `edit` and `str_replace`, and
  running a skill script, ask at every safety level, `loose` included. Answering
  "always" remembers that file for the session.
- **Foreground AI Tool Integration**: Intercepts the `!` chat agent's commands and file modifications through its safety policy. Commands the chat agent wants to run go through the same `SafetyGuard` as commands you type, including pipeline checks such as `curl | sh`, with wrappers like `sudo` looked through so the real command is the one judged. Chat tools keep workspace-root and path-traversal protections even in loose mode: `read_file`, `ls`, `search`, `edit` and `str_replace` reach the project root and the runtime skills directory, and nowhere else. `execute` runs a real shell command, so it is bounded by the safety guard and your confirmation rather than by that path sandbox.
- **Background AI tasks**: `chat_task` uses a separate explicit profile. Native
  writes within a `workspace-edit` task can run automatically; `read-only`
  cannot write automatically. See [task profiles and approvals](docs/ai-tasks.md#profiles-and-command-execution).
- **Sensitive File Policy**: Chat `read_file`, `search`, `ls`, and `edit` respect `.gitignore` (nested files, `.git/info/exclude` and global excludes included), fail closed when ignore policy cannot be evaluated, hide common secret paths, and redact secret-like content in search results.
- **Lisp Configuration**: Dynamically change the safety level at any time.
  ```lisp
  (safety-level 'strict) ; Enable confirmation for everything
  (safety-level 'normal) ; Default safety
  (safety-level 'loose)  ; Disable safety checks
  (safety-level)         ; Get current safety level
  ```
- **Environment Variable**: `SAFETY_LEVEL` reports the current level ("normal", "strict", "loose") and seeds it at startup, so `SAFETY_LEVEL=strict dogesh` starts hardened. After startup it is a readable copy: change the level with `(safety-level ...)`.

### Seeing what the assistant is about to change

A write asks first, and shows the change while it asks - the file, a `+3 -1`
summary, and the diff itself with unchanged runs collapsed:

```text
🛡️  SAFETY GUARD: AI wants to write to file: `src/main.rs` (+2 -1).
  ⋮ 27 unchanged line(s)
  line 28
  line 29
- let x = compute(a);
+ let x = compute(a)?;
+ debug!("x = {x}");
  line 31
Proceed? [y/N/a(Always)]:
```

Secrets are masked, long lines are clipped, and a large change stops after 40
lines saying how many are left. `edit`, `str_replace` and `skill_manage` all go
through it.

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
- **Configuration**: Shell configuration in Lisp with `~/.config/dogesh/config.lisp`
- **Custom Commands**: Define custom shell commands using Lisp
- **Extensibility**: Extend shell functionality with Lisp functions

### Model Context Protocol (MCP) Integration

- **MCP Client**: Connect to external Model Context Protocol servers
- **Multiple Transport**: Support for stdio and Streamable HTTP transports; legacy SSE configs are parsed but configuration-only/deprecated
- **Dynamic Tools**: Automatic discovery of MCP server tools
- **Configuration**: MCP servers are configured in `config.lisp`

### 📊 Structured Data Pipeline

Seamlessly handle structured data (JSON, CSV, Tables) within the shell pipeline — turn `ps aux` or `docker ps` into a typed table without switching to a different command language.

- **`|:` Operator**: The "Structured Pipe" operator processes command output as structured data, either with a small pipeline DSL or with a raw Lisp expression.
  ```bash
  command |: where <col> <op> <value> | select <col>... | sort [-]<col> | head [n]
  command |: (lisp-expression)
  ```
  The command output is bound to the `$_` variable; the DSL desugars to the same `table-*` calls the Lisp form uses, so both are interchangeable, and `|` inside a `|:` stage is the DSL's own separator, not a shell pipe.

- **Declarative Output Schemas**: For known commands (`ps`, `ls -l`, `df`,
  `free`, `docker ps`/`images`, `git log`/`status`, `kubectl get`), `|:`
  parses the output into a typed table automatically — no hand-written
  parsing. Column types are declared in `output-schemas/*.json` (embedded;
  `~/.config/dogesh/output-schemas/` overrides, meta schema in
  `command-output-schema.json`), so `%CPU` is a number and `-h` sizes are
  bytes (unsuffixed `df`/`free` columns keep their native units and say so in
  the column name, e.g. `avail_1k`, `total_kib`). `output-gen <command...>`
  generates a new schema by running the command, sampling its real output,
  and asking the AI to describe how to parse it — the result is always
  re-parsed against that same sample before being saved, so a schema that
  can't parse its own output is never trusted:
  ```bash
  output-gen systemctl list-units          # generate + verify + save
  output-gen --check systemctl list-units  # re-verify a saved schema, no AI call
  output-gen --audit                       # validate every schema's shape
  ```
  ```bash
  # DSL and S-expression are equivalent
  ps aux |: where cpu > 50
  ps aux |: (table-where-cmp $_ "cpu" ">" 50)

  ls -l |: sort -size
  docker ps |: where status contains Up
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

- **Table Operations** (Lisp function / DSL keyword):
  - **Viewing**: `table-display` (rich terminal UI), `table-head`/`head`, `table-tail`/`tail`
  - **Filtering**: `table-where-eq`/`is`, `table-where-ne`, `table-where-contains`/`contains`, `table-where-cmp`/`>` `>=` `<` `<=` `==` `!=` (all under `where`), `table-distinct`/`distinct`
  - **Sorting**: `table-order-by`/`sort` (`sort -col` for descending)
  - **Transformation**: `table-select`/`select` (pick columns), `table-rename`/`rename`, `table-count`/`count`
  - **Aggregation**: `table-group-by`/`group-by`, `table-count-by`/`count-by`, `table-sum`/`sum`, `table-avg`/`avg`, `table-min`/`min`, `table-max`/`max`
  - **AI Integration**: `table-to-ai-context` creates an optimized context string for LLMs
  - Column names in the DSL and in `table-where-*`/`table-order-by`/`table-select`/etc. resolve case-insensitively (`select PID` matches a `pid` column) and error with the available column names on a genuine typo, rather than silently returning nothing.

- **Examples**:
  ```bash
  # View JSON as a table
  cat data.json |: (table-display (json-parse $_))

  # Filter, sort, and pick columns with the DSL
  ps aux |: where cpu > 5 | select pid user command | sort -cpu | head 10

  # Who shows up most, and their total CPU
  ps aux |: count-by user | head 5
  ps aux |: sum cpu

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
- **Cron Jobs**: Run a shell command on an interval, a five-field cron expression, or `@daily`-style macro with `cron` — fires while a session is open, and while none is if an external tick is installed
### Project Manager

Organize and switch between workspaces efficiently with the integrated Project Manager.

- **`pm add [path] [name]`**: Register a project.
- **`pm init [name]`**: Register the current project root and show onboarding status.
- **`pm status [--json]`**: Show current project registration, activation provider, mise trust/lockfile/missing tools, Dev Container detection, runtimes, and tasks.
- **`pm list`**: List registered projects (sorted by last access).
- **`pm work <name>`**: Switch to a project and trigger hooks.
- **`pm jump` / `pj`**: Interactively select and switch to a project.
- **`pm activate --provider auto|native|mise`**: Apply safe native activation and, for trusted or conservatively classified safe mise projects, overlay `mise --no-hooks env --json`. dogesh never runs `mise trust`, installs tools, or executes hooks automatically.
- **`pm activate --dry-run`**: Preview `.env`, allowed `.envrc`, venv, and PATH changes with sensitive values masked before applying them.
- **Hooks**: Define `*on-project-switch-hooks*` in Lisp to automate environment setup.
  - Automatically triggered when entering a project directory (via `pm work`, `pj`, or `cd`).
  - Sets `DOGESH_PROJECT` environment variable to the current project name.

### Task Catalog

`task --json` exposes a stable `id/source/name/command/description/cwd` schema.
Run a task unambiguously with `task <source>:<name> -- <args>`. Static project
file parsers remain the fallback; when installed, mise, Nx, and Turbo providers
prefer their official JSON output with a bounded timeout. Results are cached by
project input metadata plus runtime/provider identity, and Nx/Turbo continue
to own graph, affected, and artifact-cache behavior.

For example, `task npm:test -- --watch` invokes `npm run test -- --watch`.
Arguments for other providers are forwarded using that runner's native syntax.

### GitHub Integration

Monitor your GitHub notifications directly from the prompt. Grouped by priority:
- `🐙`: GitHub Status (Header)
- `🔍`: Review Requested (Cyan)
- `🔔`: Mentions/Assignments (Yellow)
- `📬`: Other Notifications (Dimmed)

**Configuration**:
Set your Personal Access Token (PAT) and update interval in `config.lisp`. The prompt reads
these as Lisp variables at startup, so use `define` (not `vset`, which only sets a shell
variable the prompt does not read).

```lisp
(define *github-pat* "your_token_here")
(define *github-notify-interval* "60") ;; seconds
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
| `set`               | Set shell variables (`set KEY VALUE`, `set -o/+o pipefail`)                                                                |
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
| `chat_provider`     | Show or select the AI provider (`api_key`, `chatgpt_subscription`, `opencode_go`, `opencode_zen`); effective immediately                    |
| `chat_auth`         | Manage ChatGPT subscription sign-in, accounts, model catalog and logout                                                     |
| `chat_prompt`       | Set AI assistant system prompt                                                                                             |
| `chat_model`        | Show or set the AI model (`chat_model ""` clears the override); takes effect immediately, everywhere                      |
| `chat_reset`        | Forget the carried AI chat conversation                                                                                    |
| `chat_status`       | Show the carried AI chat conversation                                                                                      |
| `chat_task`         | Start, inspect, approve, deny or cancel one background AI task; see [the walkthrough](docs/ai-tasks.md)                       |
| `skill`             | List, review and manage the skills the AI chat runtime reads (see "Skills" above)                                          |
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
| `output-gen`        | Generate or audit `\|:` output-schemas (`--stdout`, `--force`, `--check`, `--audit`)                                       |
| `dashboard`         | Show integrated dashboard (System, Git, GitHub)                                                                            |
| `doctor`            | Diagnose config, AI, hooks, MCP, project, runtime, skills, safety, setup, and dev validation state                         |
| `ai-commit` / `aic` | Generate commit message using AI                                                                                           |
| `tm`                | Search and retrieve past command outputs                                                                                   |
| `trigger`           | Monitor file changes and execute commands (saves output to history)                                                        |
| `cron`              | Create, edit and run scheduled jobs — shell commands                                                                  |
| `notebook-play`     | Play a notebook file (execute code blocks interactively)                                                                   |
| `eproject`          | Open current project in Emacs                                                                                              |
| `eview`             | Pipe content to external editor                                                                                            |
| `magit`             | Open Magit status for the current directory                                                                                |
| `safe-run`          | Execute commands with deterministic and AI-powered safety analysis                                                         |
| `ai-watch`          | Explicitly watch a command with AI and save the summary to command blocks                                                  |

## 🧠 Lisp Functions

See [🧠 Lisp Functions](docs/lisp-functions.md#-lisp-functions).

### Core Functions

See [Core Functions](docs/lisp-functions.md#core-functions).

### Shell Integration Functions

See [Shell Integration Functions](docs/lisp-functions.md#shell-integration-functions).

### Structured Command Results

See [Structured Command Results](docs/lisp-functions.md#structured-command-results).

### Table / Structured Data Functions

See [Table / Structured Data Functions](docs/lisp-functions.md#table--structured-data-functions).

### Interactive UI Functions

See [Interactive UI Functions](docs/lisp-functions.md#interactive-ui-functions).

### Command Palette Integration

See [Command Palette Integration](docs/lisp-functions.md#command-palette-integration).

### Cron Functions

See [Cron Functions](docs/lisp-functions.md#cron-functions).

### Key Binding Functions

See [Key Binding Functions](docs/lisp-functions.md#key-binding-functions).

### Hook System Functions

See [Hook System Functions](docs/lisp-functions.md#hook-system-functions).

### MCP Management Functions

See [MCP Management Functions](docs/lisp-functions.md#mcp-management-functions).

### Suggestion Settings Functions

See [Suggestion Settings Functions](docs/lisp-functions.md#suggestion-settings-functions).

### Secret Management Functions

See [Secret Management Functions](docs/lisp-functions.md#secret-management-functions).

### PTY Control

See [PTY Control](docs/lisp-functions.md#pty-control).

## 📁 Configuration

See [📁 Configuration](docs/configuration.md#-configuration).

### config.lisp

See [config.lisp](docs/configuration.md#configlisp).

### tmux title integration

See [tmux title integration](docs/configuration.md#tmux-title-integration).

### MCP Configuration Details

See [MCP Configuration Details](docs/configuration.md#mcp-configuration-details).

#### `(mcp-clear)`

See [`(mcp-clear)`](docs/configuration.md#mcp-clear).

#### `(mcp-list-tools)`

See [`(mcp-list-tools)`](docs/configuration.md#mcp-list-tools).

#### `(mcp-add-stdio label command args env-vars cwd description [trust])`

See [`(mcp-add-stdio label command args env-vars cwd description [trust])`](docs/configuration.md#mcp-add-stdio-label-command-args-env-vars-cwd-description-trust).

#### `(mcp-add-http label url auth-header allow-stateless description [trust])`

See [`(mcp-add-http label url auth-header allow-stateless description [trust])`](docs/configuration.md#mcp-add-http-label-url-auth-header-allow-stateless-description-trust).

#### `(mcp-add-sse label url description [trust])`

See [`(mcp-add-sse label url description [trust])`](docs/configuration.md#mcp-add-sse-label-url-description-trust).

### Security & Safety

See [Security & Safety](docs/configuration.md#security--safety).

### MCP CLI Management

See [MCP CLI Management](docs/configuration.md#mcp-cli-management).

### MCP Tool Groups

See [MCP Tool Groups](docs/configuration.md#mcp-tool-groups).

## 🔧 Usage

```sh
dogesh                      # interactive shell
dogesh -c 'echo hello'       # one command
```

See [🔧 Usage](docs/usage.md#-usage).

### Basic Usage

See [Basic Usage](docs/usage.md#basic-usage).

### Smart Pipe

See [Smart Pipe](docs/usage.md#smart-pipe).

### Command Output History

See [Command Output History](docs/usage.md#command-output-history).

### Command Blocks

See [Command Blocks](docs/usage.md#command-blocks).

#### Runbooks

See [Runbooks](docs/usage.md#runbooks).

#### Block Browser

See [Block Browser](docs/usage.md#block-browser).

### Machine-readable output

See [Machine-readable output](docs/usage.md#machine-readable-output).

### Status Line

See [Status Line](docs/usage.md#status-line).

### Cron Jobs

See [Cron Jobs](docs/usage.md#cron-jobs).

#### Chat can manage cron jobs

See [Chat can manage cron jobs](docs/usage.md#chat-can-manage-cron-jobs).

### Snippets

See [Snippets](docs/usage.md#snippets).

### Insert Last Argument

See [Insert Last Argument](docs/usage.md#insert-last-argument).

### Directory Stack

See [Directory Stack](docs/usage.md#directory-stack).

### Import History

See [Import History](docs/usage.md#import-history).

### `history` Command

See [`history` Command](docs/usage.md#history-command).

### `doctor` Command

See [`doctor` Command](docs/usage.md#doctor-command).

### `help` Command

See [`help` Command](docs/usage.md#help-command).

### Project Onboarding

See [Project Onboarding](docs/usage.md#project-onboarding).

### `include` Command

See [`include` Command](docs/usage.md#include-command).

### Key Bindings

See [Key Bindings](docs/usage.md#key-bindings).

### Custom Key Bindings

See [Custom Key Bindings](docs/usage.md#custom-key-bindings).

### History Search

See [History Search](docs/usage.md#history-search).

### Fish Completion Fallback

See [Fish Completion Fallback](docs/usage.md#fish-completion-fallback).

## 💻 Command Palette

Access all shell capabilities through a unified fuzzy-search interface, similar to VS Code's Command Palette.

- **Trigger**: Press `Alt+x` to open.
- **Features**:
  - Run internal commands and setup helpers (Doctor Setup/Fix, Project Init/Status, Output History, etc.)
  - Access AI features (Explain Command, Diagnose Last Error, Check Safety, Suggest Commands, Suggest Improvement, Describe Directory). Auto-fix is `Alt+f`, not a palette entry
  - Run `safe-run` against the current input or generate completion JSON for the current command name
  - Execute Git operations
  - Extensible via the `Action` trait and Lisp `register-action`

## Command Suggestions

When a command is not found, dogesh can suggest close command names. If the current directory exposes tasks through the built-in task runner, task suggestions may also appear as `task <name>` candidates.

After a failed command, `Alt+f` first uses the local deterministic Quick Fix
engine (command/Git typos, missing upstream, occupied port, local execute bit,
and missing project runtime). It only falls back to the configured AI fixer
when no deterministic candidate exists. Suggestions only replace the input;
they are never executed automatically. The same engine is available for old
failures through `blocks fix <N> [--json] [--ai]`. Port fixes require an
explicit `:PORT` or `port PORT` diagnostic so version, PID, and errno numbers do
not become accidental kill suggestions.

Inside VS Code, dogesh emits each OSC 633 command marker once in A/B/E/C/D order,
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

Configure an [AI provider](docs/ai-integration.md), then try `! explain this project`.
For an AI task that runs while you keep using the shell:

```sh
!& Read README.md and summarize the project; do not run commands.
!&
```

Use the **same interactive shell**; bare `!&` opens its task menu. Review an approval
request before replying; `read-only` does not automatically approve commands.
See the [AI task walkthrough](docs/ai-tasks.md) for approval, denial,
cancellation and session limits.

See [🤖 AI Integration](docs/ai-integration.md#-ai-integration).

### ChatGPT subscription

Run these commands in the same `dogesh` session:

```sh
chat_provider chatgpt_subscription
chat_auth login                    # or: chat_auth login --no-browser
chat_auth status
chat_auth models
chat_model <catalog-slug>           # select a slug returned by the catalog
! hello
```

There is no default subscription model. `chatgpt`, `chatgpt-subscription`,
`openai_subscription`, and `openai-subscription` are provider aliases; `openai`
is not accepted. Provider/model changes reach AI consumers immediately. Login
alone does not switch providers. Use `chat_reset` before continuing a conversation
from another provider/account/model. To persist selection, set
`AI_CHAT_PROVIDER` and `AI_CHAT_SUBSCRIPTION_MODEL` in `config.lisp`.


If the system browser cannot open, login keeps waiting and shows a one-time
manual URL with the private identity hint removed. Open it on the same host;
a browser on another host needs forwarding for the exact loopback callback port.
Sign-in waits up to five minutes and Ctrl+C cancels. `chat_auth login --new`
registers another account/workspace; `chat_auth account <label>` selects a saved
registration and `chat_auth logout` revokes the renewable session.

Subscription inference rejects custom `AI_CHAT_BASE_URL`/`OPENAI_BASE_URL`;
clear those settings explicitly before selecting subscription. Authentication
management remains available to recover from invalid inference settings.
No API-key fallback or token import from other apps occurs. Catalog inclusion
does not guarantee entitlement. See [subscription details](docs/design/ai/subscription.md).

For a noninteractive request, set `AI_CHAT_PROVIDER=chatgpt_subscription` and
`AI_CHAT_SUBSCRIPTION_MODEL=<catalog-slug>` in the invoking environment (or
`config.lisp`), then run `dogesh -c '! hello'` after user-operated sign-in.

### OpenCode Go / Zen

OpenCode Go / Zen are also available as dedicated providers. Set
`OPENCODE_GO_API_KEY` or `OPENCODE_ZEN_API_KEY` (or the explicit shared
`OPENCODE_API_KEY`), then run `chat_provider opencode_go glm-5.3` or
`chat_provider opencode_zen glm-5.3`. Run `chat_reset` before continuing an
existing conversation after switching. `chat_model` changes the selected
provider's separate model; `chat_provider models opencode_go` (or
`opencode_zen`) lists the verified offline Chat Completions subset. There is
no default model. Responses, Messages and Google model routes are currently
unsupported. `chat_provider`, `chat_status` and `doctor ai` show the route.
Go uses subscription quotas; if Console **Use balance** is enabled, requests
may spend balance after quota. Zen / Console uses prepaid, pay-as-you-go billing.
There is no automatic switch between them. Generic OpenAI keys, URLs and
models are preserved and never used by these dedicated routes. See
[provider configuration](docs/design/ai/env-vars.md#opencode-go--zen).

### API key

See [API key](docs/ai-integration.md#api-key).

### Long-running commands in `!` chat

See [Long-running commands in `!` chat](docs/ai-integration.md#long-running-commands-in--chat).

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
   - `scripts/check-ai-guidance.sh` after changing `AGENTS.md`, `CLAUDE.md`, `docs/ai/`, `.claude/`, or runtime Skill installer guidance
   - `scripts/check-project-consistency.py` after changing workspace manifests, `README.md`, or `LICENSE`
   - `doctor validate` can suggest the focused commands from your current `git status`
6. Keep runtime Skill copies current when Skill guidance changes:
   `scripts/install-runtime-skills.sh --check-installed --target codex --profile codex-core`
7. Commit your changes (`git commit -m 'Add amazing feature'`)
8. Push to the branch (`git push origin feature/amazing-feature`)
9. Open a Pull Request

## 📄 License

This project is licensed under the MIT license - see the [LICENSE](LICENSE) file for details.

## 🙏 Acknowledgments

- Built with [Rust](https://www.rust-lang.org/)
- Uses [skim](https://github.com/lotabout/skim) for fuzzy finding
- Inspired by modern shells like Fish and Zsh
- Includes an embedded Lisp interpreter for extensibility
- Integrates with Model Context Protocol (MCP) for AI-assisted tool access
