# doge-shell (dsh)

A modern, feature-rich shell written in Rust with an integrated Lisp interpreter and AI-powered command completion.

## 🐕 Overview

doge-shell (dsh) is a simple yet powerful shell that combines traditional shell capabilities with modern features like AI-assisted command completion, frecency-based history, and an embedded Lisp scripting environment.

## ✨ Features

### Core Shell Features

- **Interactive Command Line**: Full-featured interactive shell with readline-like functionality
- **Command Execution**: Execute external commands, built-in commands, and shell scripts
- **Background Processing**: Run commands in background with `&` and manage jobs
- **Pipes and Redirections**: Support for pipes (`|`), input/output redirection (`>`, `>>`, `<`), and error redirection
- **Signal Handling**: Proper handling of signals like SIGINT, SIGQUIT, SIGTSTP
- **Subshells**: Support for command substitution and process substitution
- **Safe Paste**: Bracketed paste support ensures pasted multi-line text is not executed immediately

### Advanced Features

- **Command Palette**: Unified interface for accessing shell commands and features with `Alt+x`
- **Frecency-based History**: Intelligent command history using frecency scoring (frequency + recency)
- **Context-Aware History**: Prioritizes commands based on the current directory or Git repository context
- **Directory Navigation**: Smart directory history and jump with `z` command
- **Path Management**: Dynamic PATH management with `add_path` command
- **Job Control**: Background job management with `jobs`, `bg`, `fg` commands
- **Aliases**: Command aliasing with `alias` command
- **Variables**: Environment variable management with `var`, `set` commands
- **Abbreviations**: Define and use abbreviations with `abbr` command

### Completion & UI

- **Context-Aware Completion**: Intelligent tab completion for commands, files, and options
- **Skim Integration**: Fuzzy finding interface for completion using [skim](https://github.com/lotabout/skim)
- **History Search**: Interactive history search with Ctrl+R
- **Command Abbreviations**: Define and use abbreviations with `abbr` command
- **AI-Powered Completion**: OpenAI integration for intelligent command completion suggestions
- **Right Prompt**: Displays command execution status and duration on the right side
- **Inline Argument Explainer**: Displays real-time descriptions of command arguments and options below the prompt as you type
- **Transient Prompt**: Automatically collapses the prompt after command execution to keep the terminal clean

### 🛡️ Safety Guard

The Safety Guard protects against unintended execution of potentially destructive commands.

- **Safety Levels**:
  - `Loose`: No restrictions.
  - `Normal` (Default): Requires confirmation for common dangerous commands (`rm`, `mv`, `cp`, `dd`, `mkfs`, `format`).
  - `Strict`: Requires confirmation for **all** commands.
- **AI Tool Integration**: Automatically intercepts AI-generated commands and file modifications, requiring explicit user approval.
- **Lisp Configuration**: Dynamically change the safety level at any time.
  ```lisp
  (safety-level "strict") ; Enable confirmation for everything
  (safety-level "normal") ; Default safety
  (safety-level "loose")  ; Disable safety checks
  (safety-level)          ; Get current safety level
  ```
- **Environment Variable**: `SAFETY_LEVEL` reflects the current safety level (e.g., "normal", "strict").

### Lisp Interpreter

- **Embedded Lisp**: Built-in Lisp interpreter for shell scripting
- **Configuration**: Shell configuration in Lisp with `~/.config/dsh/config.lisp`
- **Custom Commands**: Define custom shell commands using Lisp
- **Extensibility**: Extend shell functionality with Lisp functions

### Model Context Protocol (MCP) Integration

- **MCP Client**: Connect to external Model Context Protocol servers
- **Multiple Transport**: Support for stdio, HTTP, and SSE transports
- **Dynamic Tools**: Automatic discovery of MCP server tools
- **Configuration**: MCP servers are configured in `config.lisp`

### Other Features

- **Git Integration**: Commands for Git operations (`ga`, `gco`, `glog`, etc.)
- **Auto-Correction Suggestion**: Suggests similar commands when a typo is detected (e.g., "Did you mean: git ?" when typing `gti`)
- **Command Output History**: Capture command output with `|>` operator and reference it with `$OUT` variable
- **Command Timing Statistics**: Track execution time and frequency with `timing` command
- **UUID Generation**: Built-in UUID generation with `uuid` command
- **URL Shortening**: URL shortening with `dmv` command
- **Web Server**: Built-in static file server with `serve` command
- **Configuration Reload**: Runtime configuration reloading with `reload` command
- **Trigger Command**: Monitor file changes matching a glob pattern and automatically execute commands. Results are captured in the [output history](#command-output-history).
### Project Manager

Organize and switch between workspaces efficiently with the integrated Project Manager.

- **`pm add [path] [name]`**: Register a project.
- **`pm list`**: List registered projects (sorted by last access).
- **`pm work <name>`**: Switch to a project and trigger hooks.
- **`pm jump` / `pj`**: Interactively select and switch to a project.
- **Hooks**: Define `*on-project-switch-hooks*` in Lisp to automate environment setup.
  - Automatically triggered when entering a project directory (via `pm work`, `pj`, or `cd`).
  - Sets `DSH_PROJECT` environment variable to the current project name.

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

| Command | Description |
|---------|-------------|
| `exit` | Exit the shell |
| `cd` | Change directory |
| `history` | Show command history |
| `z` | Jump to frequently used directories (use `-i` or `--interactive` for selection, `-` for previous directory, `-l` for list) |
| `jobs` | Show background jobs |
| `fg` | Bring job to foreground |
| `bg` | Send job to background |
| `lisp` | Execute Lisp expressions |
| `set` | Set shell variables |
| `var` | Manage shell variables |
| `read` | Read input into a variable |
| `abbr` | Configure abbreviations |
| `alias` | Configure command aliases |
| `export` | Set export attribute for shell variables |
| `chat` | Chat with AI assistant |
| `chat_prompt` | Set AI assistant system prompt |
| `chat_model` | Set AI model |
| `gh-notify` | View GitHub notifications interactively |
| `glog` | Git log with interactive selection |
| `gco` | Git checkout with interactive branch selection |
| `ga` | Git add with interactive file selection |
| `add_path` | Add path to PATH environment variable |
| `serve` | Start a static file server |
| `uuid` | Generate UUIDs |
| `dmv` | URL shortener |
| `reload` | Reload shell configuration |
| `timing` | Show command execution statistics |
| `out` | Display captured command output history |
| `include` | Execute a bash script and import environment variables |
| `mcp` | Manage MCP servers (status, connect, disconnect) |
| `gpr` | GitHub Pull Request checkout with interactive selection |
| `gwt` | Git Worktree management (add, list, remove) |
| `pm` | Project Manager (add, list, remove, work, jump) |
| `pj` | Jump to a project (alias for `pm jump`) |
| `help` | Show help information |
| `comp-gen` | Generate command completion using AI (`--stdout`, `--check`) |
| `dashboard` | Show integrated dashboard (System, Git, GitHub) |
| `ai-commit` / `aic` | Generate commit message using AI |
| `tm` | Search and retrieve past command outputs |
| `trigger` | Monitor file changes and execute commands (saves output to history) |
| `notebook-play` | Play a notebook file (execute code blocks interactively) |
| `eproject` | Open current project in Emacs |
| `eview` | Pipe content to external editor |
| `magit` | Open Magit status for the current directory |

## 🧠 Lisp Functions

The embedded Lisp interpreter includes many built-in functions:

### Core Functions

- `print` - Print a value
- `is_null`, `is_number`, `is_symbol`, `is_boolean`, `is_procedure`, `is_pair` - Type checking
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
- `getenv` - Get environment variables
- `vset` - Set shell variables
- `add_path` - Add paths to PATH
- `number->string` - Convert number to string
- `string-append` - Concatenate strings
- `allow-direnv` - Configure direnv roots
- `edit` - Open a file in the external editor

### Interactive UI Functions

### Interactive UI Functions

- `selector` - Open an interactive fuzzy selection menu with custom prompt and items.
  - Usage: `(selector "Prompt" '("Item1" "Item2") [multi])`
  - If `multi` is true, returns a list of selected items. Default is false (single selection).

### Command Palette Integration

- `register-action` - Register a custom action in the Command Palette.
  - Usage: `(register-action "Name" "Description" "function-name")`

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
- `mcp-add-http` - Add an MCP server with HTTP transport
- `mcp-add-sse` - Add an MCP server with SSE transport
- `mcp-list` - List registered MCP servers
- `mcp-list-tools` - List all available MCP tools
- `chat-execute-clear` - Clear execute tool allowlist
- `chat-execute-add` - Add command(s) to execute tool allowlist (accepts multiple commands)

### Suggestion Settings Functions

- `set-suggestion-mode` - Set suggestion mode (`ghost` or `off`)
- `set-suggestion-ai-enabled` - Enable/disable AI-powered suggestions

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

```lisp
;; Example configuration
(setq prompt "🐶 > ")
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

;; Add MCP server with HTTP transport
;; Parameters: label, URL, authentication header (optional), allow stateless (optional), description (optional)
(mcp-add-http 
  "remote-http-service"               ; label
  "https://example.com/mcp"           ; URL
  '()                                 ; authentication header (NIL = no auth)
  '()                                 ; allow stateless (NIL = false)
  "Remote HTTP MCP service"           ; description
)

;; Add MCP server with SSE transport
;; Parameters: label, URL, description (optional)
(mcp-add-sse 
  "streaming-service"                 ; label
  "https://example.com/sse"           ; URL
  "SSE-based MCP service"             ; description
)

;; Chat execute allowlist - commands that can be executed by AI assistant
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

Adds an MCP server that communicates via Server-Sent Events.

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


### Import History

Import command history from other shells:

```bash
# Import from fish shell
dsh import fish

# Import from bash with custom path

dsh import bash --path /path/to/bash_history
```

### `include` Command

Source a bash script and import its environment variables into the current shell session.
Useful for loading `.env` files or setup scripts.

```bash
include setup.sh
```

### Key Bindings

- `Tab` - Context-aware completion
- `Ctrl+R` - Interactive history search
- `Ctrl+C` - Cancel current command (press twice to exit shell)
- `Ctrl+L` - Clear screen
- `Ctrl+D` - Show exit hint (use `exit` to leave)
- `Ctrl+K` - Delete from cursor to end of line
- `Ctrl+U` - Delete from cursor to beginning of line
- `Ctrl+W` - Delete word backward
- `Alt+x` - Open Command Palette
- `Esc` (double press) - Toggle `sudo` prefix for the current command
- `Ctrl+x Ctrl+e` - Edit current input in external editor (`$VISUAL` or `$EDITOR`)
- `Alt+Enter` - Execute command in background
- `Ctrl+Space` - Force AI suggestion
- `Alt+[` / `Alt+]` - Rotate through suggestions

## 💻 Command Palette

Access all shell capabilities through a unified fuzzy-search interface, similar to VS Code's Command Palette.

- **Trigger**: Press `Alt+x` to open.
- **Features**:
  - Run internal commands (Clear Screen, Reload Config, etc.)
  - Access AI features (Explain, Fix, etc.)
  - Execute Git operations
  - Extensible via `Action` trait and Lisp interface (coming soon)

## 🤖 AI Integration

The shell includes AI-powered command completion using OpenAI. To use this feature:

1. Set your OpenAI API key in the environment:

   ```bash
   export AI_CHAT_API_KEY="your-api-key-here"
   ```

2. The shell will automatically provide command suggestions when available.

3. Use `!` prefix to chat with the AI directly:

   ```bash
   !explain how to use the grep command
   ```

   chat "How do I compress a directory with tar?"

   ```

5. **Smart Pipe Expansion (`|?`)**:

   Describe how you want to filter or process data in natural language, and let AI expand it into shell commands.

   ```bash
   # Type:
   ls -l |? sort by size and take top 5<Tab>
   
   # Expands to:
   ls -l | sort -rn -k 5 | head -n 5
   ```

6. **Generative Command (`??`)**:
   Describe what you want to do in natural language at the start of the line, and let AI generate the command for you.

   ```bash
   # Type:
   ?? undo last git commit<Enter>
   
   # Expands to:
   git reset --soft HEAD~1
   ```

7. **Auto-Fix (Error Recovery) (`Alt+f`)**:
   If a command fails, press `Alt+f` to have AI suggest a fix for the last failed command.

   ```bash
   # Type a wrong command:
   git stats
   # Command fails...
   
   # Press Alt+f, command input becomes:
   git status
   ```

8. **Smart Git Commit (`Alt+c`)**:
   Stage your changes, then press `Alt+c` to generate a commit message based on the diff.

   ```bash
   git add .
   # Press Alt+c, command input becomes:
   git commit -m "feat: implement new features..."
   ```

9. **AI Output Pipe (`|!`)**:
   Pipe command output to AI for analysis with a natural language query.

   ```bash
   # Analyze log files:
   docker logs app |! "summarize the errors"
   
   # Find specific information:
   kubectl get pods |! "which pods are failing?"
   
   # Get file statistics:
   ls -la |! "what is the largest file?"
   ```

10. **Error Diagnosis (`Alt+d`)**:
    When a command fails, press `Alt+d` to have AI diagnose the error and suggest fixes.

    ```bash
    gti status  # Typo - command fails
    # Press Alt+d, AI analyzes the error and suggests:
    # "The command 'gti' was not found. Did you mean 'git'?"
    ```

11. **AI Quick Actions (`Alt+a`)**:
    Press `Alt+a` to open a menu of AI-powered actions including:
    - Explain Command
    - Suggest Improvement
    - Check Safety
    - Diagnose Error
    - Describe Directory
    - Suggest Commands
    
12. **Safe Run (`safe-run`)**:
    Execute commands with AI-powered safety analysis. Useful for auditing potential risky commands or inspecting output before piping.

    ```bash
    # Analyze and execute a command
    safe-run rm -rf tmp/

    # Inspect content before piping (e.g., curl | sh)
    safe-run curl https://example.com/install.sh | sh
    ```

    - **Analysis**: AI checks for destructive operations or malicious patterns.
    - **Content Inspection**: For pipe operations, you can inspect the captured output (preview shown on stderr) before allowing it to pass to the next command.
    - **Confirmation**: Required for execution.


## 📁 Project Structure

- `dsh/` - Main shell executable and core implementation
- `dsh-builtin/` - Built-in commands
- `dsh-frecency/` - Frecency-based history management
- `dsh-types/` - Shared data structures
- `dsh-openai/` - OpenAI integration
- `completions/` - Command completion definitions

## 🤝 Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Make your changes
4. Add tests if applicable
5. Run tests (`cargo test`)
6. Commit your changes (`git commit -m 'Add amazing feature'`)
7. Push to the branch (`git push origin feature/amazing-feature`)
8. Open a Pull Request

## 📄 License

This project is licensed under the MIT/Apache-2.0 license - see the [LICENSE](LICENSE) file for details.

## 🙏 Acknowledgments

- Built with [Rust](https://www.rust-lang.org/)
- Uses [skim](https://github.com/lotabout/skim) for fuzzy finding
- Inspired by modern shells like Fish and Zsh
- Includes an embedded Lisp interpreter for extensibility
- Integrates with Model Context Protocol (MCP) for AI-assisted tool access
