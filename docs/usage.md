# 🔧 Usage

[← README](../README.md)

### Basic Usage

```bash
# Start the shell interactively
dogesh

# Execute a single command
dogesh -c "echo 'Hello, World!'"

# Execute an inline Lisp program
dogesh -l "(print \"Hello from Lisp!\")"

# Execute a Lisp script file
dogesh lisp ./script.lisp
dogesh lisp ./script.lisp -- first second
```

Both inline (`-l`) and file (`dogesh lisp FILE`) modes evaluate every
top-level form in order as one program. Inside a program, `*argv0*` is the
script path (`"dogesh"` for `-l`) and `*argv*` is the list of remaining
arguments — `(length *argv*)` gives the argument count, and option-looking
arguments are kept verbatim (use `--` to separate them from the file path).
The file path resolves against the current working directory, `config.lisp`
loads before the program as usual, and the final value prints unless it is
NIL. A successful evaluation exits 0; read, parse, or runtime failures exit
non-zero with the script path in the diagnostic.

> **Renamed from `dsh`:** the binary is now `dogesh`, the config directory is
> `~/.config/dogesh`, project skills live in `<project>/.dogesh/skills`, and
> `DSH_*` environment variables are now `DOGESH_*` (old names are not read).
> Migrating from an older checkout:
> `mv ~/.config/dsh ~/.config/dogesh`, rename each project's `.dsh` to
> `.dogesh`, and replace `DSH_` with `DOGESH_` in your hook scripts and settings.

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
`blocks list --json`, `timing --json`, and `cron list`/`cron status`/`cron history`
(each with `--json`) write exactly one JSON value to
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

An optional line pinned to the bottom row, showing scheduled tasks,
background jobs, git state and GitHub notifications:

```
⏱ 3 failing 1   ⚙ 2 jobs    main ●4 ↑1   🐙 5
```

**Off by default.** Enable it in `config.lisp`:

```lisp
(pref-status-line t)
```

`DOGESH_STATUS_LINE=0` forces it off regardless, and it stays off on a non-terminal or a
terminal shorter than three rows.

It works by reserving a scroll region (DECSTBM) so the bottom row sits outside the
scrolling area — the prompt, command output and job notices are unaffected. The region
is released whenever something else needs the whole screen (`Ctrl+R`, `Ctrl+O`, `Alt+x`,
tab completion, `$EDITOR`, and any foreground command) and restored afterwards, and it
is always released on exit.

Everything shown is read from caches the shell already maintains, so the status line
never adds work to the prompt. It is off by default because DECSTBM support varies
between terminals.

### Cron Jobs

`cron` runs a shell command on a schedule:

```bash
cron add --name fetch 5m git fetch --all
cron add --name prs --on change '0 9-17 * * mon-fri' gh pr list
cron add --quiet 30s 'df -h /'
cron list                               # id, schedule, state, next run, last result
cron history fetch                      # recent runs
cron logs fetch                         # a run's recorded stdout/stderr (each stream clamped to 8 KiB)
cron rm prs
cron pause                              # stop every job, keeping the definitions
cron resume
```

Unlike the old `sched` builtin it replaced, jobs persist across restarts (a SQLite store
under `$XDG_STATE_HOME/dogesh/cron`, not `config.lisp`) and understand real wall-clock
schedules, not just intervals:

| Form | Example | Meaning |
| --- | --- | --- |
| Interval | `30s` / `5m` / `1h` | Every N seconds/minutes/hours, from the previous run. 5s-24h. |
| Cron expression | `0 9 * * mon-fri` | Five fields (minute hour day-of-month month day-of-week), on the local wall clock. |
| Macro | `@hourly` / `@daily` / `@weekly` / `@monthly` / `@yearly` | Shorthand for a fixed cron expression. |
| `@reboot` | `@reboot` | Once per interactive session runner start; never fires from an external tick. |
| `@manual` | `@manual` | Only an explicit `cron run` / `cron run --now`; never fires on its own. |

**Always quote a cron expression** — `cron add */5 * * * * git fetch` is glob-expanded by
the shell before dogesh ever sees it. `cron add` detects the common shapes of this mistake
and names the fix.

**Two ways a job fires**, and either is enough on its own:

- **A dogesh session is open.** Each interactive session runs its own scan for due jobs and
  starts them — nothing to install, works the moment you `cron add`. Closing every
  session pauses that clock until one opens again, unless:
- **An external tick is installed.** `cron setup` prints a crontab line, a systemd user
  timer, or a launchd agent (auto-detected, or pick with `--crontab`/`--systemd`/`--launchd`)
  that calls `dogesh -c "cron tick"` on a schedule — this is what makes a job fire while
  logged out. `cron setup` only prints; it never installs anything itself.

Whether a run is worth reporting is set by `--on`, same meanings as before:

| `--on`      | Reports                                              |
| ----------- | ---------------------------------------------------- |
| `never`     | Nothing (`--quiet` is a shorthand)                   |
| `failure`   | The command started failing, and again when it recovers |
| `change`    | The output differs from the previous run              |
| `both`      | Either of the above — **default**                    |
| `always`    | Every run                                            |

Outcomes are recorded either way — `cron history <name>` and `cron status` are the source
of truth. (A live above-prompt notice and desktop notification, as `sched` had, are not
yet wired for cron: a run finishes in its own separate process, which the open session is
not directly watching.)

**Commands run under `sh -c`**, in the directory registered with the job, with stdin on
`/dev/null` and in their own process group — shell aliases, abbreviations, builtins and
Lisp functions are **not** available inside them. Write out the full command, or wrap it
in a script.

A run that overruns its own interval is skipped rather than stacked, and each run is
killed after its `--timeout` (60s by default, capped to the interval for an interval
schedule). A job that was due while nothing was running to run it does **not** fire once
per missed slot on catch-up — everything older than `--catchup` (one hour by default)
collapses into a single run.

Two sessions (or a session and an external tick) racing to run the same job is resolved
by the store itself, not by hoping only one thing is ever watching: whichever claims the
row first runs it, and the other finds nothing left to do.

#### Chat can manage cron jobs

The `!` chat agent has a `cron_manage` tool for the job lifecycle
(`list`/`show`/`history`/`logs`/`incidents`/`status`/`doctor` for reading;
`create`/`update`/`pause`/`resume`/`remove`/`run`/`ack` for writing) through a
tool call instead of the CLI — `execute` cannot reach `cron` itself, since it is
a builtin, not a shell command. It does not cover `setup`, `notepad`, or `tick`.
A job it creates always starts paused.
Every write (`create`/`update`/`pause`/`resume`/`remove`/`run`/`ack`) still
asks a person first, the same as any other tool that changes something.

Full CLI reference, schedule grammar, external-tick setup for each OS, and a
symptom-to-cause debugging table live in the `dsh-cron` skill
(`docs/ai/skills/dsh-cron/`) — install it for the `!` chat agent with
`scripts/install-runtime-skills.sh --target dogesh --profile dogesh-user`.

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

Import command history from another shell (currently fish only):

```bash
# Import from fish shell
dogesh import fish

# Import from a non-default fish history path
dogesh import fish --path /path/to/fish_history
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
dual-write is enabled with `DOGESH_ATUIN_DUAL_WRITE=1`; failures run off the prompt
path and never block a command.

### `doctor` Command

Inspect the current shell setup and project context.

```bash
# Run all diagnostics
doctor

# Focus on one area
doctor ai
doctor hooks
doctor project
doctor skills
doctor setup
doctor fix
doctor validate

# Show command help
doctor --help
```

`doctor` reports on configuration files, AI settings, AI chat hook configuration, MCP connection counters, project marker files, common developer runtimes found in `PATH`, performance/cache state, the skills this shell would load (and anything wrong with them), runtime Skill drift, setup readiness, and focused validation commands for changed files. `doctor fix` creates safe missing setup files and directories such as `config.lisp`, runtime skills, and completion directories.

### `help` Command

Search built-in commands and show command-specific usage.

```bash
help
help doctor
help project
help --search ai
```

### Project Onboarding

Register the current project and inspect what dogesh can activate or run.

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
- `Alt+e` - Explain the current command with AI
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

Set `DOGESH_HISTORY_PICKER=skim` to fall back to the previous skim-based interface.

### Fish Completion Fallback

TAB completion calls `fish -c 'complete -C ...'` automatically when `fish` is available in `PATH`. Leave `DOGESH_COMPLETION_FISH_FALLBACK` unset for auto mode, set it to `1`, `true`, `yes`, or `on` to force it on, or set it to `0`, `false`, `no`, or `off` to disable it. This fallback runs with a timeout, is cached with other external completion results, and is merged below built-in JSON and project-aware dynamic candidates.

Built-in dynamic providers also complete live resource identifiers for supported tools such as `gh`, `glab`, `argocd`, cloud CLIs, Vault, Nomad, Terraform/OpenTofu, rclone, and restic. They invoke only read-only listing commands, run off the prompt thread, use a five-second command timeout and a 30-second cache, and quietly return no candidates when a CLI is missing, unauthenticated, or unavailable. Provider diagnostics report cache age and errors but never include tokens, passwords, or fetched secret contents. Additional providers cover 1Password (`op item get/edit/delete`), Vagrant installed boxes (`vagrant box remove/repackage`, `vagrant init`), and `.envrc` files plus their containing directories discovered from the working directory chain for `direnv allow/deny/edit`.

Linux operations use the same dynamic completion path for systemd units and machines, journal identifiers, network namespaces and links, firewall profiles, storage resources, processes, kernel modules, audit keys, SELinux modules and booleans, wireless devices, device subsystems, and the login shells listed in `/etc/shells`. Arch Linux definitions cover `yay`, `paru`, `paccache`, `pacdiff`, `pactree`, `pacman-conf`, `pacman-key`, `pkgctl`, repository database tools, devtools chroot commands, `namcap`, and `snapper`. `yay` and `paru` deliberately reuse only the local pacman databases: sync operations complete repository packages, removal operations complete installed packages, and completion never performs an AUR network search. Snapper configurations and snapshots, pacman repositories, and mkinitcpio presets are discovered with read-only local probes. A `systemctl.unit` provider scope narrows candidates to a single unit type, so `systemd-run --slice` offers slices rather than every unit. Candidates reflect the current host while read-only probes remain cached and run outside the prompt thread.

Developer toolchains are covered as well: `rustup` components and targets, crates installed with `cargo install`, `cargo` test and bench targets, `cargo nextest`, `cargo watch`, `sccache`, `bacon`, Jujutsu (`jj`), PDM, Pipenv, Pyright, Biome, golangci-lint, GoReleaser, Delve, Meson, watchexec, and ghq, in addition to the existing language and editor tools. Dynamic candidates include Jujutsu bookmarks, revisions, and workspaces; Meson targets; golangci-lint linters; ghq repositories; and scripts or jobs declared by bacon, PDM, and Pipenv. Definitions that live in project files — `bacon.toml` jobs, `pyproject.toml` PDM scripts, `Pipfile` scripts, `noxfile.py` sessions, `tox` environments, `hatch` environments, `pre-commit` hook ids, and `just` recipes and `make` targets through the shared project task provider — are parsed directly, so completion never imports or evaluates project code. Local CLI probes use a one-second cache and two-second error backoff; missing tools, permission failures, timeouts, and malformed output safely produce no dynamic candidates.

