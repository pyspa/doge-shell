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

### Advanced Features
- **Frecency-based History**: Intelligent command history using frecency scoring (frequency + recency)
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

### Lisp Interpreter
- **Embedded Lisp**: Built-in Lisp interpreter for shell scripting
- **Configuration**: Shell configuration in Lisp with `~/.config/dsh/config.lisp`
- **Custom Commands**: Define custom shell commands using Lisp
- **Extensibility**: Extend shell functionality with Lisp functions

### Model Context Protocol (MCP) Integration
- **MCP Client**: Connect to external Model Context Protocol servers
- **Multiple Transport**: Support for stdio, HTTP, and SSE transports
- **Dynamic Tools**: Automatic discovery of MCP server tools
- **Configuration**: MCP servers can be configured in both config.lisp and config.toml

### Other Features
- **Git Integration**: Commands for Git operations (`gco`, `glog`, etc.)
- **UUID Generation**: Built-in UUID generation with `uuid` command
- **URL Shortening**: URL shortening with `dmv` command
- **Web Server**: Built-in static file server with `serve` command
- **Configuration Reload**: Runtime configuration reloading with `reload` command

## 🔧 Built-in Commands

The shell includes many built-in commands:

| Command | Description |
|---------|-------------|
| `exit` | Exit the shell |
| `cd` | Change directory |
| `history` | Show command history |
| `z` | Jump to frequently used directories |
| `jobs` | Show background jobs |
| `fg` | Bring job to foreground |
| `bg` | Send job to background |
| `lisp` | Execute Lisp expressions |
| `set` | Set shell variables |
| `var` | Manage shell variables |
| `read` | Read input into a variable |
| `abbr` | Configure abbreviations |
| `alias` | Configure command aliases |
| `chat` | Chat with AI assistant |
| `chat_prompt` | Set AI assistant system prompt |
| `chat_model` | Set AI model |
| `glog` | Git log with interactive selection |
| `gco` | Git checkout with interactive branch selection |
| `add_path` | Add path to PATH environment variable |
| `serve` | Start a static file server |
| `uuid` | Generate UUIDs |
| `dmv` | URL shortener |
| `reload` | Reload shell configuration |
| `help` | Show help information |

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
- `vset` - Set shell variables
- `add_path` - Add paths to PATH
- `allow-direnv` - Configure direnv roots

### MCP Management Functions
- `mcp-clear` - Clear all MCP servers
- `mcp-add-stdio` - Add an MCP server with stdio transport
- `mcp-add-http` - Add an MCP server with HTTP transport
- `mcp-add-sse` - Add an MCP server with SSE transport
- `chat-execute-clear` - Clear execute tool allowlist
- `chat-execute-add` - Add command(s) to execute tool allowlist (accepts multiple commands)

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
```

### MCP Configuration Details

MCP (Model Context Protocol) allows the shell to connect to external services that provide tools for AI assistants. You can configure MCP servers in your `config.lisp` file using these functions:

#### `(mcp-clear)`
Removes all currently configured MCP servers.

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

### config.toml (MCP Configuration)

For MCP server configuration, you can also create a `~/.config/dsh/config.toml` file:

```toml
[mcp]
# Define MCP servers that connect via stdio
servers = [
  { label = "local-tools", description = "Local MCP tools", transport = { type = "stdio", command = "/path/to/server", args = [] } },
  { label = "remote-service", description = "Remote HTTP MCP service", transport = { type = "http", url = "https://example.com/mcp" } },
  { label = "streaming-service", description = "SSE MCP service", transport = { type = "sse", url = "https://example.com/sse" } }
]
```

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

### Import History
Import command history from other shells:
```bash
# Import from fish shell
dsh import fish

# Import from bash with custom path
dsh import bash --path /path/to/bash_history
```

### Key Bindings
- `Tab` - Context-aware completion
- `Ctrl+R` - Interactive history search
- `Ctrl+C` - Cancel current command (press twice to exit shell)
- `Ctrl+L` - Clear screen
- `Ctrl+D` - Show exit hint (use `exit` to leave)

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

4. Use the `chat` command for extended conversations:
   ```bash
   chat "How do I compress a directory with tar?"
   ```

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
