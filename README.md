# doge-shell

Doge-shell is a high-performance shell written in Rust. Its fast and efficient design makes it a perfect choice for users who value speed and performance.

## Features

- Command completion like fish shell
- Scripting support for Lisp
- High-speed performance, thanks to being written in Rust
- Prompt with Git status display, similar to starship
- Comes bundled with a WASM runtime, allowing you to run WASM (WASI-compliant) binaries
- Comprehensive set of built-in commands for shell operations
- AI integration with OpenAI ChatGPT
- Advanced job control system
- Frecency-based directory navigation

## Installation

To install doge-shell, follow these steps:

```shell
$ git clone https://github.com/pyspa/doge-shell.git
$ cd doge-shell
$ cargo build --release
$ cargo install
```

Usage
To start using doge-shell, simply run:

```
$ dsh
```

## Built-in Commands

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

### AI Integration Commands

#### `chat <message>`
Integrates with OpenAI ChatGPT API for AI-powered assistance within the shell. Requires `OPENAI_API_KEY` environment variable to be set.

```bash
🐕 < chat "Explain how to use git rebase"
🐕 < chat "Write a bash script to backup files"
```

**Requirements:**
- Set `OPENAI_API_KEY` environment variable with your OpenAI API key
- Internet connection for API communication

#### `chat_prompt <prompt_template>`
Sets a custom prompt template for ChatGPT interactions. The prompt template provides context for all subsequent chat commands.

```bash
🐕 < chat_prompt "You are a helpful Linux system administrator"
🐕 < chat "How do I check disk usage?"
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

## config.lisp

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
    (vlet ((slct (sh "git branch --all | grep -v HEAD | sk | tr -d ' ' "))
           (branch (sh "echo $slct | sed 's/.* //' | sed 's#remotes/[^/]*/##'")))
          (sh "git checkout $branch")))

(fn fkill (arg)
    (vlet ((q arg)
           (slct (sh "ps -ef | sed 1d | sk -q $q | awk '{print $2}' ")))
        (sh "kill -TERM $slct")))

```

License
Doge-shell is released under the MIT license. For more information, see LICENSE.
