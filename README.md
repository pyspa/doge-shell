# 🐕 doge-shell

Doge-shell is a modern, high-performance shell written in Rust that combines the speed of native code with advanced features like AI integration, Lisp scripting, and WebAssembly runtime support. Designed for developers and power users who value both performance and extensibility.

## ✨ Key Features

### 🚀 Performance & Architecture
- **High-speed execution** - Written in Rust with zero-cost abstractions
- **Modular design** - Multi-crate workspace architecture for maintainability
- **Memory safety** - Rust's ownership system prevents common shell vulnerabilities
- **Concurrent processing** - Tokio-based async runtime for responsive I/O

### 🧠 AI Integration
- **OpenAI ChatGPT integration** - Use `!` prefix for AI-powered assistance
- **Streaming responses** - Real-time AI output with "Thinking..." indicator
- **Configurable prompts** - Customize AI behavior for different contexts
- **Full shell integration** - AI responses support redirection and piping

### 🎯 Advanced Completion System
- **Multi-layered completion** - Commands, files, and context-aware suggestions
- **JSON-based definitions** - Extensible command completion via JSON files
- **Fuzzy matching** - Smart completion with scoring and ranking
- **History integration** - Suggestions based on command history patterns
- **Real-time display** - Interactive completion with visual feedback

### 🔧 Lisp Scripting Engine
- **Built-in Lisp interpreter** - Full-featured scripting environment
- **Shell integration** - Direct access to shell functions and variables
- **User-defined functions** - Extend shell functionality with custom Lisp code
- **Configuration system** - Customize shell behavior via `config.lisp`

### 🌐 WebAssembly Runtime
- **WASI-compliant execution** - Run WebAssembly binaries natively
- **Wasmer integration** - High-performance WASM runtime
- **Sandboxed execution** - Secure execution environment for WASM modules

### 📁 Smart Navigation
- **Frecency-based directory jumping** - `z` command for intelligent navigation
- **Path history tracking** - Learns from your directory usage patterns
- **Fuzzy directory matching** - Quick access to frequently used locations

### ⚡ Advanced Job Control
- **Process group management** - Proper job control with signal handling
- **Background/foreground switching** - Full job control like traditional shells
- **Job monitoring** - Real-time status tracking of background processes
- **Signal propagation** - Correct signal handling for process groups

## 🏗️ Architecture

Doge-shell is built as a modular Rust workspace with six specialized crates:

```
doge-shell/
├── dsh/              # Main shell binary and core logic
├── dsh-builtin/      # Built-in commands implementation
├── dsh-frecency/     # Frecency-based navigation system
├── dsh-types/        # Shared types and context definitions
├── dsh-wasm/         # WebAssembly runtime integration
└── dsh-openai/       # OpenAI API client and integration
```

### Core Components
- **Shell Engine** - Command parsing, execution, and process management
- **Completion Engine** - Multi-layered completion with fuzzy matching
- **Lisp Interpreter** - Built-in scripting environment for configuration
- **Job Controller** - Advanced process and signal management
- **AI Client** - OpenAI integration with streaming support
- **WASM Runtime** - WebAssembly execution environment

## 📦 Installation

### Prerequisites
- Rust 2024 Edition (latest stable)
- Git for cloning the repository

### Build from Source

```shell
$ git clone https://github.com/pyspa/doge-shell.git
$ cd doge-shell
$ cargo build --release
$ cargo install --path dsh
```

### Quick Start

```shell
$ dsh
🐕 < echo "Welcome to doge-shell!"
Welcome to doge-shell!
```

### Configuration

Create a `config.lisp` file in your configuration directory to customize the shell:

```lisp
;; ~/.config/dsh/config.lisp
(alias "ll" "ls -la")
(alias "g" "git")

;; Custom function for git branch switching
(fn gco ()
    (vlet ((branch (sh "git branch | sk | tr -d ' '")))
          (sh "git checkout $branch")))
```

## 🤖 AI Integration

Doge-shell features seamless OpenAI ChatGPT integration for AI-powered assistance directly in your terminal.

### Getting Started with AI

1. Set your OpenAI API key:
```shell
export OPENAI_API_KEY="your-api-key-here"
```

2. Use the `!` prefix to interact with AI:
```shell
🐕 < ! How do I find large files in Linux?
Thinking...
You can find large files in Linux using several methods:

1. Using `find` command:
   find /path/to/search -type f -size +100M

2. Using `du` with sort:
   du -ah /path | sort -rh | head -20
...
```

### AI Features

- **Streaming responses** - See AI output in real-time
- **Shell integration** - Redirect AI output to files or pipe to other commands
- **Configurable prompts** - Set custom system prompts for different contexts
- **Context awareness** - AI understands shell environment and common tasks

### AI Command Examples

```shell
# Get help with commands
🐕 < ! Explain the difference between grep and awk

# Generate scripts
🐕 < ! Write a bash script to backup my home directory > backup.sh

# Troubleshoot issues
🐕 < ! Why am I getting permission denied when running docker?

# Set custom prompt for specific domain
🐕 < chat_prompt "You are a DevOps expert specializing in Kubernetes"
🐕 < ! How do I debug a failing pod?
```

## 🎯 Advanced Completion System

Doge-shell features a sophisticated multi-layered completion system that provides intelligent suggestions as you type.

### Completion Features

1. **Command Completion** - Completes commands from your PATH
2. **File/Directory Completion** - Smart filesystem navigation
3. **Subcommand Completion** - Context-aware command options
4. **History-based Suggestions** - Learn from your command patterns
5. **Fuzzy Matching** - Find what you need with partial matches

### How It Works

- **TAB key** - Trigger completion menu
- **Real-time suggestions** - See history matches as you type
- **JSON-based definitions** - Extensible completion for any command
- **Priority system** - Most relevant suggestions first

### Completion Priority

1. **Subcommand/Option completion** (when command is already entered)
2. **Command/File completion** (for new commands)
3. **History-based suggestions** (matching previous commands)

### Custom Completion

Add completion definitions in JSON format:

```json
{
  "command": "myapp",
  "description": "My custom application",
  "subcommands": [
    {
      "name": "start",
      "description": "Start the application",
      "options": [
        {
          "long": "--port",
          "description": "Port number",
          "takes_value": true,
          "value_type": {"type": "Number"}
        }
      ]
    }
  ]
}
```

Save as `completions/myapp.json` in your doge-shell directory.

## 🔧 Built-in Commands

Doge Shell provides a comprehensive set of built-in commands for efficient shell operations. These commands are implemented in Rust for optimal performance and are tightly integrated with the shell's features.

### Core Shell Commands

#### `exit`
Terminates the current shell session gracefully.

```bash
🐕 < exit
```

#### `cd [directory]`
Changes the current working directory. Supports various path formats:
- Absolute paths (starting with `/`)
- Home directory paths (starting with `~`)
- Relative paths
- No argument defaults to home directory

```bash
🐕 < cd /usr/local/bin          # Absolute path
🐕 < cd ~/Documents             # Home directory path
🐕 < cd ../parent               # Relative path
🐕 < cd                         # Go to home directory
```

#### `history`
Displays the command history, showing previously executed commands for reference.

```bash
🐕 < history
```

### Navigation and Directory Management

#### `z [pattern]`
Provides frecency-based directory navigation, similar to the popular `z` utility. Quickly jump to frequently and recently visited directories by partial name matching.

```bash
🐕 < z proj                     # Jump to most frecent directory matching "proj"
🐕 < z                          # Show frecency-ranked directories
```

### Job Control Commands

#### `jobs`
Lists all active background jobs in the current shell session, showing job IDs, status, and command information.

```bash
🐕 < jobs
┌─────┬──────┬─────────┬─────────────────┐
│ job │ pid  │ state   │ command         │
├─────┼──────┼─────────┼─────────────────┤
│ 1   │ 1234 │ Running │ long_process    │
│ 2   │ 1235 │ Stopped │ vim file.txt    │
└─────┴──────┴─────────┴─────────────────┘
```

#### `fg [job_spec]`
Brings a background job to the foreground for interactive execution. Job specification can be:
- Job number: `1`, `2`, etc.
- `%1`, `%2` for job references
- `%+` for current job, `%-` for previous job
- Empty for most recent job

```bash
🐕 < fg                         # Foreground most recent job
🐕 < fg 1                       # Foreground job 1
🐕 < fg %+                      # Foreground current job
```

#### `bg [job_spec]`
Resumes a stopped job in the background, allowing it to continue execution while you use the shell.

```bash
🐕 < bg                         # Resume most recent stopped job
🐕 < bg 1                       # Resume job 1 in background
🐕 < bg %2                      # Resume job 2 in background
```

### Scripting and Configuration

#### `lisp <s-expression>`
Evaluates Lisp s-expressions using the shell's integrated Lisp interpreter. Used for advanced scripting and shell configuration.

```bash
🐕 < lisp '(+ 1 2 3)'           # Evaluate arithmetic expression
🐕 < lisp '(alias "ll" "ls -la")' # Define alias using Lisp
```

#### `set [options] <key> <value>`
Sets shell variables or environment variables. Supports both local shell variables and exported environment variables.

**Options:**
- `-x, --export`: Export as environment variable (available to child processes)
- `-h, --help`: Show help information

```bash
🐕 < set MY_VAR "hello world"   # Set local shell variable
🐕 < set -x PATH "/new/path:$PATH" # Export environment variable
🐕 < set --export API_KEY "secret" # Export with long option
```

#### `var`
Displays all current shell variables in a formatted table.

```bash
🐕 < var
┌─────────┬──────────────┐
│ key     │ value        │
├─────────┼──────────────┤
│ MY_VAR  │ hello world  │
│ USER    │ username     │
└─────────┴──────────────┘
```

#### `read <variable_name>`
Reads input from stdin and stores it in the specified shell variable. Commonly used in shell scripts for interactive input collection.

```bash
🐕 < echo "Enter your name:" && read name
🐕 < echo "Hello $name"
```

#### `alias [name[=command]]`
Manages shell aliases with support for setting, listing, and querying aliases.

```bash
🐕 < alias                      # List all aliases
🐕 < alias ll="ls -la"          # Set an alias
🐕 < alias ll                   # Show specific alias
```

### AI Integration Commands

#### `chat [options] <message>`
Integrates with OpenAI ChatGPT API for AI-powered assistance within the shell. Supports model selection and custom prompts.

**Options:**
- `-m, --model <model>` - Use specific OpenAI model for this request

```bash
# Use default model (o1-mini)
🐕 < chat "Explain how to use git rebase"

# Use specific model
🐕 < chat -m gpt-4 "Complex reasoning task"
🐕 < chat --model o1-preview "Advanced analysis needed"

# Write scripts and get help
🐕 < chat "Write a bash script to backup files"
```

**Requirements:**
- Set `OPENAI_API_KEY` environment variable with your OpenAI API key
- Internet connection for API communication
- Optional: Set `OPENAI_MODEL` environment variable for default model

#### `chat_prompt <prompt_template>`
Sets a custom prompt template for ChatGPT interactions. The prompt template provides context for all subsequent chat commands.

```bash
🐕 < chat_prompt "You are a helpful Linux system administrator"
🐕 < chat "How do I check disk usage?"
```

#### `chat_model [model_name]`
Manages the default OpenAI model for ChatGPT interactions. When called without arguments, shows the current model.

```bash
# Show current default model
🐕 < chat_model
Current OpenAI model: o1-mini (default)

# Set new default model
🐕 < chat_model gpt-4
OpenAI model set to: gpt-4

# Available models include:
# - o1-mini (fast, cost-effective) [default]
# - o1-preview (advanced reasoning)
# - gpt-4 (balanced performance)
# - gpt-3.5-turbo (fastest, cheapest)
```

### Utility Commands

#### `add_path <directory>`
Adds a directory to the beginning of the PATH environment variable, giving it the highest priority for command lookup. Supports tilde expansion for home directory references.

```bash
🐕 < add_path ~/bin             # Add ~/bin to PATH
🐕 < add_path /usr/local/bin    # Add /usr/local/bin to PATH
```

#### `uuid`
Generates and outputs a random UUID (Universally Unique Identifier) using UUID version 4 for maximum uniqueness.

```bash
🐕 < uuid
550e8400-e29b-41d4-a716-446655440000
```

### Usage Notes

- All built-in commands support I/O redirection and piping
- Commands integrate seamlessly with the shell's job control system
- Error messages are displayed on stderr with appropriate exit codes
- Commands respect the shell's variable expansion and environment
- AI commands require proper API configuration and internet connectivity

### Examples of Advanced Usage

```bash
# Combine commands with pipes and redirection
🐕 < history | grep git > git_commands.txt

# Use AI integration with output redirection
🐕 < chat "Explain Docker basics" > docker_guide.txt

# Job control workflow
🐕 < long_running_command &     # Start in background
🐕 < jobs                       # Check job status
🐕 < fg 1                       # Bring to foreground

# Variable management
🐕 < set PROJECT_DIR ~/my-project
🐕 < cd $PROJECT_DIR
🐕 < add_path $PROJECT_DIR/bin
```

## 📝 Configuration with config.lisp

Users can extend the functionality with their own lisp scripts.
An example is shown below.

```lisp

;; Define alias

(alias "a" "cd ../")
(alias "aa" "cd ../../")
(alias "aaa" "cd ../../../")
(alias "aaaa" "cd ../../../../")
(alias "ll" "exa -al")
(alias "cat" "bat")
(alias "g" "git")
(alias "gp" "git push")
(alias "m" "make")

;; It has a direnv equivalent.
(allow-direnv "~/repos/github.com/pyspa/doge-shell")

;; User functions
(fn gco ()
  (value-let ((slct (sh "git branch --all | grep -v HEAD | sk | tr -d ' ' "))
              (branch (sh "echo $slct | sed 's/.* //' | sed 's#remotes/[^/]*/##'")))
    (sh "git checkout $branch")))

(fn fkill (arg)
  (value-let ((q arg)
              (slct (sh "ps -ef | sed 1d | sk -q $q | awk '{print $2}' ")))
    (sh "kill -TERM $slct")))

```

## 📄 License

Doge-shell is released under the MIT license. For more information, see LICENSE.
