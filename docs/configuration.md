# 📁 Configuration

[← README](../README.md)

### config.lisp

Create a `~/.config/dogesh/config.lisp` file to configure your shell:

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

;; Trust: every MCP server is `untrusted` unless you explicitly pass
;; `"trusted"` as the last argument. An untrusted server's tools always ask
;; before running at the default `normal` safety level (unless you allowlisted
;; that exact call); a trusted server's tools may run unasked when the server
;; explicitly annotates them read-only, and its command-execution tools are
;; still judged as the command they carry.

;; Add MCP server with stdio transport (for local executable servers)
;; Parameters: label, command path, arguments list, environment variables list, working directory (optional), description (optional), trust (optional: "trusted" or "untrusted", default "untrusted")
(mcp-add-stdio 
  "local-dev-tools"                    ; label
  "/path/to/your/mcp-server"          ; command
  '("arg1" "arg2")                    ; arguments list
  '(("ENV_VAR1" "value1") ("ENV_VAR2" "value2"))  ; environment variables list
  '()                                 ; working directory (NIL = current directory)
  "Local development tools via stdio"  ; description
  "trusted"                           ; trust (omit or "untrusted" for the default)
)

;; Add MCP server with Streamable HTTP transport
;; Parameters: label, URL, authentication header (optional), allow stateless (optional), description (optional), trust (optional: "trusted" or "untrusted", default "untrusted")
(mcp-add-http 
  "remote-http-service"               ; label
  "https://example.com/mcp"           ; URL
  '()                                 ; authentication header (NIL = no auth)
  '()                                 ; allow stateless (NIL = false)
  "Remote HTTP MCP service"           ; description
  "untrusted"                         ; trust (default when omitted)
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
  (print "About to execute:")
  (print command)
)

(defun my-post-exec-func (command exit-code)
  (print "Executed:")
  (print command)
  (print "Exit code:")
  (print exit-code)
)

(defun my-chdir-func ()
  (print "Changed directory")
)

;; Add functions to the appropriate hook lists
;; Note: add-hook expects the base name without asterisks - it adds them internally
(add-hook 'pre-prompt-hooks 'my-pre-prompt-func)
(add-hook 'pre-exec-hooks 'my-pre-exec-func)
(add-hook 'post-exec-hooks 'my-post-exec-func)
(add-hook 'on-chdir-hooks 'my-chdir-func)
```

### tmux title integration

`dogesh` updates the terminal title while a foreground command is running. To let tmux reflect that in the window name, add this to `~/.tmux.conf`:

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

#### `(mcp-add-stdio label command args env-vars cwd description [trust])`

Adds an MCP server that communicates via standard input/output streams.

- `label`: A unique identifier for the server
- `command`: Path to the server executable
- `args`: List of command-line arguments to pass to the server
- `env-vars`: List of (key value) pairs for environment variables
- `cwd`: Working directory for the server (or NIL for current directory)
- `description`: Optional description of the server
- `trust`: Optional `"trusted"` or `"untrusted"` (default `"untrusted"`).
  Only a trusted server's explicit read-only annotations may auto-run at the
  `normal` safety level; anything else is a configuration error.

Example:

```lisp
(mcp-add-stdio 
  "git-tools" 
  "/usr/local/bin/git-mcp-server" 
  '("--verbose") 
  '(("GIT_AUTHOR_NAME" "Your Name")) 
  '() 
  "Git utility tools"
  "trusted"
)
```

#### `(mcp-add-http label url auth-header allow-stateless description [trust])`

Adds an MCP server that communicates via HTTP requests.

- `label`: A unique identifier for the server
- `url`: The HTTP endpoint for the server
- `auth-header`: Authentication header value (or NIL)
- `allow-stateless`: Whether to allow stateless operations (or NIL)
- `description`: Optional description of the server
- `trust`: Optional `"trusted"` or `"untrusted"` (default `"untrusted"`).
  Only a trusted server's explicit read-only annotations may auto-run at the
  `normal` safety level; anything else is a configuration error.

Example:

```lisp
(mcp-add-http 
  "remote-api" 
  "https://api.example.com/mcp" 
  '("Bearer your-token-here") 
  '() 
  "Remote API server"
  "untrusted"
)
```

#### `(mcp-add-sse label url description [trust])`

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

### Security & Safety

- **Execution Confirmation**: MCP tool calls are judged by the same `SafetyGuard` as everything else, whether they come from `!` chat or from a shell-side AI action, and both share one set of approvals. At `loose` they run unasked; at `strict` every MCP call asks. At `normal`, MCP servers are untrusted by default and every call asks unless you allowlisted that exact call - the tool's name and its `readOnlyHint` are both server-controlled, so neither opens the gate on its own. Pass `"trusted"` as the last `mcp-add-*` argument only for servers you run yourself: a trusted server's tools may run unasked when explicitly annotated read-only, while its command-execution tools (`bash`, `run_command`, ...) are still judged as the command they carry. Answering "always" remembers that exact call for the session.
- **Disconnecting takes effect**: `mcp disconnect <label>` removes that server's tools from what the agent is offered and refuses a call to them until `mcp connect <label>`.

### MCP CLI Management

You can also manage MCP servers interactively using the `mcp` command:

- `mcp status`, `mcp s`: Show connection status of all servers.
- `mcp connect <label>`, `mcp c`: Connect to a specific server.
- `mcp disconnect [label]`, `mcp d`: Disconnect from a server (or all).
- `mcp list`, `mcp l`: List registered servers.
- `mcp tools`, `mcp t`: List available tools.

### MCP Tool Groups

Each MCP server's tools form a tool group named for the server. A connected
server is not the same as AI exposure: `mcp group disable <group>` hides that
group's schemas from AI while keeping the connection up, and the model can
load a hidden group on demand when the task needs it.

- `mcp group list`, `mcp groups`: List groups and their exposure state.
- `mcp group show <group>`: Show one group's tools and state.
- `mcp group enable <group>`: Offer a group's tools to AI again.
- `mcp group disable <group>`: Hide a group's tools from AI (keeps connection).
- `mcp tools --active`: List only the tools currently exposed to AI.
- `mcp tools --group <name>`: List one group's tools.

Hiding idle groups keeps every AI request small: `doctor mcp` reports active
versus registered tool counts and warns while the active footprint is high.

