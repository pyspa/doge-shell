# 🤖 AI Integration

[← README](../README.md)

The shell supports an OpenAI API key (default) or official Sign in with ChatGPT subscription usage.

For a complete AI task running alongside shell input, use the
[`chat_task` walkthrough](ai-tasks.md). For a single long-running command started
inside `!` chat, see [command jobs](#long-running-commands-in--chat).

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
does not guarantee entitlement. See [subscription details](../docs/design/ai/subscription.md).

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
[provider configuration](design/ai/env-vars.md#opencode-go--zen).

### API key

To use the API-key provider:

1. Set your OpenAI API key in the environment:

   ```bash
   export AI_CHAT_API_KEY="your-api-key-here"
   ```

   Optional settings:

   `OPENAI_API_KEY` and `OPEN_AI_API_KEY` are accepted as fallbacks, as are
   `OPENAI_BASE_URL` and `OPENAI_MODEL`. Every setting below is read as a shell
   variable first and from the process environment second, so `config.lisp` can
   set any of them without an `export`.

   | Variable | Default | Purpose |
   | --- | --- | --- |
   | `AI_CHAT_MODEL` | `gpt-5-mini` | Model used for chat and AI actions. Also settable at runtime with `chat_model <model>` (or `chat_model ""` to clear it); the change reaches every AI path immediately - `!` chat, command palette actions, ghost text, `ai-watch`, `blocks explain\|fix`, `output-gen` - no restart needed |
   | `AI_SUMMARY_MODEL` | chat model | Model used to summarize a long conversation |
   | `AI_CHAT_BASE_URL` | `https://api.openai.com/v1/` | OpenAI-compatible endpoint |
   | `AI_CHAT_ALLOW_INSECURE_HTTP` | off | Allow an `http://` base URL (local models) |
   | `AI_CHAT_TIMEOUT_SECS` | `180` | Total per-request timeout |
   | `AI_CHAT_REASONING_EFFORT` | unset (`none` on `tools` requests to a `gpt-5`/`o1`/`o3`/`o4` model, provider default otherwise) | `reasoning_effort` sent with every request, e.g. `none`/`minimal`/`low`/`medium`/`high` (provider-dependent). Set explicitly and it is sent on every request, including JSON generation (`safe-run`'s risk check, comp-gen), so `none` trades reasoning quality there too |
   | `AI_CHAT_SESSION_TTL_SECS` | `1800` | Idle timeout for the carried conversation; the clock restarts on each turn that finishes (a rewound failed turn does not restart it). `0` disables carrying it forward |
   | `AI_CHAT_CONTEXT_TOKEN_BUDGET` | `100000` | Prompt tokens before the conversation is summarized |
   | `AI_CHAT_TURN_TOKEN_BUDGET` | unset | Stop one `!` turn once it has spent this many tokens |
   | `AI_CHAT_STREAM` | on | Stream `!` chat's answer as it is generated; `0`/`false`/`off`/`no` prints it once at the end instead |
   | `AI_CHAT_EXECUTE_ALLOWLIST` | unset | Extra entries for the `execute` tool allowlist, merged with `config.lisp` and `~/.config/dogesh/openai-execute-tool.json` |
   | `AI_CHAT_EXECUTE_YIELD_MS` | `10000` | How long a `!` chat waits for a command before handing the model a job handle to poll instead of a result. `0` yields immediately; 60000 is the ceiling |
   | `AI_CHAT_EXECUTE_TIMEOUT_MS` | `600000` | Default `timeout_ms` for a `!` chat command. An unattended task keeps its own default |
   | `DOGESH_EXECUTE_TOOL_CONFIG` | `~/.config/dogesh/openai-execute-tool.json` | Path of that JSON allowlist file |
   | `AI_MESSAGE_LANG` | unset | Language for AI answers - `!` chat, `Alt+d`, `Alt+e`, `aic`, `safe-run`, `ai-watch`, `blocks explain`. Requests whose answer is parsed as JSON are left alone |
   | `CHAT_PROMPT` | unset | Extra operator instructions appended to the `!` system prompt (`chat_prompt`) |
   | `AI_CHAT_PROJECT_SKILLS` | on | Read `<project>/.dogesh/skills` at all; `0`/`false`/`off`/`no` keeps a repository's skills out of the prompt |
   | `AI_CHAT_SKILL_STAGING` | `task` | Whether `skill_manage` writes land immediately or wait in `skill pending`: `task` (only when an unattended task has no `--write` grant for the target) / `always` / `off` |
   | `AI_CHAT_SKILL_REFLECT` | off | Send one tool-free request after a long turn proposing a skill from what it did; never writes directly |
   | `AI_CHAT_SKILL_REFLECT_MIN_TOOLS` | `5` | Tool calls a turn needs before the reflection reviewer considers it |
   | `AI_CHAT_SKILL_REFLECT_MODEL` | `AI_SUMMARY_MODEL`, then the chat model | Model used for the reflection request |
   | `AI_CHAT_SKILL_AUTO_ARCHIVE_DAYS` | off | Archive an agent-written, unpinned personal skill once unread this many days; unset or `0` disables it |
   | `AI_CHAT_HOOKS` | on | Run AI chat hooks; `0`/`false`/`off`/`no` stops `ai-hooks.json` from being read |
   | `AI_CHAT_HOOK_TURN_BUDGET_MS` | unset | Ceiling on the wall time one `!` turn waits for hooks; `0` removes it |
   | `AI_CHAT_MCP_TOOL_MODE` | `auto` | MCP model surface for `!` chat: `auto` bridges large catalogs (discoverable schemas over 12 KiB) behind `tool_search`/`tool_describe`/`tool_call` and keeps small ones eager; `eager`/`bridge` force either side |
   | `DOGESH_AI_HOOKS_CONFIG` | `~/.config/dogesh/ai-hooks.json` | Path of the hook configuration |

   Transient failures (429, 5xx, timeouts) are retried with backoff, honouring
   `Retry-After`. An optional request field the endpoint rejects outright is
   dropped once and the request retried.

   The entire current OpenAI reasoning lineup (any `gpt-5*` point release -
   including `gpt-5.6-luna` and its siblings - plus `o1`, `o3`, `o4`) rejects
   `tools` requests unless `reasoning_effort` is `"none"`, so a `tools`
   request to one of those models sends `reasoning_effort: none` from the
   first attempt with no configuration needed - this also covers the shell's
   own default model, `gpt-5-mini`. Requests without `tools`
   (summarization, `safe-run`'s JSON generation) are untouched, so they keep
   whichever reasoning quality is configured or the model's own default. Set
   `AI_CHAT_REASONING_EFFORT` explicitly to override this for every request
   (including `tools` ones); if that value still conflicts with `tools` on
   the endpoint, it gets corrected to `none` after one extra round-trip, the
   same way an unrecognised model outside the known lineup would be.

2. **The AI features are off by default.** An API key on its own sends no
   requests: inline suggestions, automatic fixes and inline explanations each
   need enabling in `config.lisp`.

   ```lisp
   (set-suggestion-ai-enabled t) ; AI inline suggestions (ghost text)
   (set-auto-fix-enabled t)      ; let Alt+f fall back to AI
   (pref-ai-explanation t)       ; inline AI explanation of the current command
   (vset "AI_MESSAGE_LANG" "Japanese") ; answer in this language
   ```

   `!` chat, `Alt+d`, `Alt+e`, `aic`, `safe-run` and `ai-watch` are explicit
   actions and work without any of these.

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

   **Proactive failure hints** (on by default): when a command fails, dogesh
   automatically shows the deterministic quick-fix as ghost text with a short
   reason next to the prompt — accept it with `Tab` or `Alt+f`. No AI request
   is sent unless `set-auto-fix-enabled` is on. Interrupted commands
   (`Ctrl-C`), "no match" exits from `grep`/`diff`-style commands, and the
   same failure repeating stay quiet; for a pipeline the exit status belongs
   to its last segment, so `cat f | grep x` finding nothing stays quiet too.
   `(pref-failure-hint nil)` in `config.lisp` turns off the whole automatic
   path, automatic AI fixes included — `Alt+f` and `Alt+d` keep working on
   demand. When nothing deterministic applies the shell says nothing, unless
   `(pref-diagnose-hint t)` asks it to point at `Alt+f`/`Alt+d` instead; that
   hint is a line of text, not an AI request.

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

    While it is waiting you see how long it has been going, and `Esc` (or `Ctrl+C`) gives up
    on it. The same is true of the AI actions in the command palette, `Alt+s`, and
    `ai-watch`'s summary: the shell stays responsive instead of going quiet until the
    provider answers.

7. **Safe Run (`safe-run`)**:
    Execute commands with deterministic prechecks and AI-powered safety analysis. Useful for auditing potential risky commands or inspecting output before piping.

    ```bash
    # Analyze and execute a command
    safe-run rm -rf tmp/

    # Inspect content before piping (e.g., curl | sh)
    safe-run -- 'curl https://example.com/install.sh | sh'
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
    `ls`, `read_file`, `str_replace`, `edit`, `execute` and `skill_manage`.

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
      it after `timeout_ms` (600s by default for an interactive `!` turn; `AI_CHAT_EXECUTE_TIMEOUT_MS` overrides it), so a build or a dev server cannot wedge
      the shell. What the guard cannot read before it runs is refused rather than
      approved: command substitution (`$(...)`, backticks, `<(...)`, `>(...)`) and subshells,
      because evaluating them is what a safety check must not do; compound
      statements (`{ ...; }`, `for`, `if`, `while`), because the commands inside one
      cannot be classified; a line the parser cannot finish; and any way of handing
      an interpreter code it cannot see (`sh -c`, `eval`, a shell reading stdin).
      Write those as separate commands.
    - The command is checked by the same `SafetyGuard` that covers what you type, and
      every stage of a pipeline is judged, and wrappers are looked through, so both
      `true | rm -rf ...` and `sudo rm -rf ...` are judged as `rm`. At the
      default `normal` safety level that means the destructive commands the guard knows
      about ask first and everything else runs; set `(safety-level 'strict)` to be asked
      about every command the allowlist does not already cover. Anything that writes to
      a file by redirection asks as well.
    - An allowlisted command skips the question entirely - that is what putting it on
      the list means - so keep `(chat-execute-add ...)` to commands you are content for
      the AI to run unattended. Entries match by token prefix (`cargo test` covers
      `cargo test -p foo`); an "always" answer given at a prompt matches only that exact
      command line.
    - Tools can read and write within the project root and the skills directories.
      `.gitignore` is honoured the way git honours it, nested files included. Reading a
      skill is the one exception, so a repository that ignores `.dogesh/` can still carry
      project skills the assistant reads; writing one still goes through `skill_manage`.
    - Long tool output is truncated in the middle, so the end of a build or test log -
      where the error is - still reaches the model.
    - `edit`, `str_replace`, `skill_manage` and skill scripts always ask for
      confirmation, at every safety level. That includes scripts under a project's
      `.dogesh/skills/`. Answering "always" applies to that file for the rest of the
      session, so a long editing run is one question per file rather than one per
      edit. Deleting a skill is asked separately: an "always" given for writing a
      file does not authorise removing it.
    - With `AI_CHAT_SKILL_STAGING` (default `task`), a `skill_manage` write that
      would otherwise stall an unattended task with no `--write` grant for the
      target is queued instead of asked about, so the task keeps running. Review it
      with `skill pending`/`skill diff`/`skill approve`/`skill reject` - see
      "Skills" below.

10. **Conversation continuity**:
    Consecutive `!` turns continue the same conversation, so follow-up questions work and
    the assistant does not re-explore the project every time. Each turn that continues one
    prints a dim `session: continuing ...` line, and one that starts a new conversation
    prints why - except the very first `!` in a shell, and every turn while
    `AI_CHAT_SESSION_TTL_SECS=0`, which print nothing (there is no previous conversation to
    explain away). It restarts when you move to a different project (it follows you within
    one project root, so `cd src` keeps it), when the operator prompt or language changes
    (changing provider, model or account instead requires `chat_reset` before sending more history), when an MCP
    server is connected or disconnected, after `AI_CHAT_SESSION_TTL_SECS` of idle time
    (default 1800), or on `chat_reset`. Installing a skill - or having the assistant write
    one - does not restart it. `chat_status` shows what is currently carried without
    discarding it, and re-checks idle time, prompt identity, MCP context and project scope.

    A turn that does not finish - Ctrl-C, an API error, the tool-iteration cap, or
    `AI_CHAT_TURN_TOKEN_BUDGET` - is removed from the conversation, but everything before it
    is kept: only that one turn is lost, not the whole conversation. The carried conversation
    is also persisted in the shell's state directory, subject to its idle TTL. Background
    `chat_task` conversations remain memory-only and cannot be resumed after shell exit.

### Long-running commands in `!` chat

A command the assistant runs is a managed job. If it finishes within
`AI_CHAT_EXECUTE_YIELD_MS` (10 seconds by default) you get what you always got: the exit
code, stdout and stderr, in one round trip. If it does not - a cold `cargo build`, a long
test run - the assistant gets a job handle instead and follows it with `job_status` /
`job_output` / `job_cancel`, while the command's output keeps appearing on your screen as
it is produced.

```sh
! build this in release mode and tell me what breaks
#  [1/100] 0s
#  🔧 [Tool] execute ({"command":"cargo build --release"})
#     Compiling dsh-builtin v0.1.0
#  job 8f3a2c1d still running (cargo build --release); the assistant can follow it with job_status
#  [2/100] 11s · 1 job(s) running
```

Jobs outlive the turn that started them, so the assistant can start a build, answer you,
and check on it next turn. They do not outlive the conversation: `chat_reset`, a change of
project, or a turn the shell cannot carry forward all stop them, as does leaving the shell.

```sh
chat_status    # lists running jobs with their pid and elapsed time
chat_reset     # forgets the conversation and stops its jobs
kill -- -12345 # stop one job by the process group `chat_status` shows
```

The shell's own `jobs` and `kill` builtins do not list these - those manage the jobs *you*
started, and these belong to the assistant.

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

13. **Project instructions and Skills**:

    `!` loads scoped `AGENTS.md` guidance after session trust. It walks from the
    repository boundary to the working/target directory, applies parent before
    child, and refreshes before requests and file operations. Source paths and
    failures are diagnostic; `doctor ai` lists candidates without prompting.
    Set `AI_CHAT_PROJECT_INSTRUCTIONS=off` to disable. Limits, symlink handling,
    nested repositories, and command/MCP limitations are documented in
    [project instructions](../docs/design/ai/project-instructions.md).

    **Skills**:
    A skill is a short `SKILL.md` the assistant reads when it is relevant - and can
    write for itself, so a procedure worked out once is not worked out again.

    The chat runtime reads them from three places, most specific first:

    | Scope | Directory | What belongs there |
    |---|---|---|
    | Project | `<project>/.dogesh/skills/` | procedures tied to this repository |
    | Project, shared | `<project>/.agents/skills/` | the same, kept where every agent working in the checkout can find them |
    | Personal | `~/.config/dogesh/skills/` (`$XDG_CONFIG_HOME/dogesh/skills` when set) | anything portable |

    A more specific root shadows a less specific one of the same name, so
    `.dogesh/skills` wins over `.agents/skills`, which wins over your personal root.
    Only the name, the path and the frontmatter `description` reach the system
    prompt; the body is read on demand, so keep the description a trigger
    ("Use when …") and move long detail into `references/`. Frontmatter keys dogesh
    does not use - `allowed-tools`, `license` and the rest - are ignored rather
    than rejected, so a skill written for another agent loads as it is; dogesh does
    not enforce `allowed-tools`, and a skill cannot narrow what the assistant may
    do any more than it can widen it.

    **`.agents/skills/` is read, never written.** It is shared with other tools,
    and a directory this shell does not own is not a place for it to leave files,
    so `skill_manage` with `scope: "project"` always writes to `.dogesh/skills/`.
    There is no personal equivalent: if you keep portable skills in
    `~/.agents/skills`, point the personal root at them
    (`ln -s ~/.agents/skills ~/.config/dogesh/skills`) and they load with one trust
    story rather than two.

    ```bash
    skill list                 # scope, read count, last use
    skill show rust-bisect     # render its SKILL.md
    $EDITOR $(skill path rust-bisect)
    skill remove old-notes
    ```

    **The assistant writes them too.** After a task that took many steps, or after you
    correct it, it may offer to record the lesson with its `skill_manage` tool. Every
    write asks first, and the answer is remembered per file for the rest of the session.
    A write that would leave the skill without a usable `name` or `description` - the
    two things the prompt actually shows - is refused before you are asked about it,
    rather than silently dropping out of the prompt on the next turn.
    `skill list` shows which skills are actually being read, so the ones that are not
    can be removed - every summary costs tokens on every turn, and `doctor skills`
    warns once there are more than eight.

    **Reviewing a write before it lands.** `AI_CHAT_SKILL_STAGING` controls whether a
    `skill_manage` write goes to disk right away or waits for you: `task` (default)
    only queues one when an unattended task has no `--write` grant covering the target -
    the one case that used to stall the task on a question nobody could answer -
    `always` queues every write, interactive included, and `off` restores today's
    behaviour. A queued write is never counted as a read or a write, and it never
    touches trust or the prompt fragment until it is applied.

    ```bash
    skill pending               # what is waiting, and why
    skill diff rust-bisect      # what it would change
    skill approve rust-bisect   # apply it - asks first, then writes through
                                 # the same path skill_manage itself uses
    skill reject rust-bisect    # discard it
    ```

    `delete` is never queued - there is nothing to review, and it is the one change
    here with no undo. `skill approve` refuses a proposal whose target has moved
    since it was staged (edited by hand, or by an approval that landed first) rather
    than overwrite it; reject it and ask again.

    **An optional reviewer at the end of a long turn.** `AI_CHAT_SKILL_REFLECT` (off
    by default) sends one extra, tool-free request after a turn that used at least
    `AI_CHAT_SKILL_REFLECT_MIN_TOOLS` (default 5) tool calls, asking whether anything
    from it is worth saving. It never writes directly - a proposal it makes is queued
    exactly like one `skill_manage` would stage, and it never rewrites a skill it did
    not see the body of during that turn. A failed or empty reviewer call never
    changes the turn's own answer.

    **Archiving what nobody reads any more.** `skill archive <name>` drops a personal
    skill out of the prompt without deleting it - `skill unarchive` brings it back,
    and `skill pin` exempts one from ever being swept automatically. Only personal
    (`user`-scope) skills can be archived: archiving is what keeps a repository's own
    trust decision (the digest of what it advertises) from changing whenever a
    *personal* skill's status changes, so it deliberately stops at the door of
    anything a project ships. `AI_CHAT_SKILL_AUTO_ARCHIVE_DAYS` (off unless set to a
    positive number) archives an agent-written, unpinned personal skill once it has
    gone unread that many days - the same staleness `skill list` and `doctor skills`
    already report, just acted on instead of only mentioned.

    **The first time you use `!` in a repository that ships skills, dogesh asks.** Their
    descriptions would otherwise be in every prompt before you had decided anything,
    and the assistant on the other side of that prompt can run commands. Answer `y`
    for this shell session or `a` to remember the repository; adding a skill, or
    rewording one, asks again - as does dogesh itself widening how much of a
    description it keeps, which happens rarely and re-asks every trusted
    repository once. You are asked once per directory, so a checkout
    carrying both `.dogesh/skills/` and `.agents/skills/` asks twice and refusing one
    leaves the other alone. `skill trust` shows the current answers and
    `skill untrust` takes them back. Under an unattended task nothing is asked and an
    untrusted repository is simply not read - an unattended run should be the more
    careful one, not the more trusting one.

    **Naming a skill outright.** Start the message with `@name` to load it whether or
    not the assistant would have picked it:

    ```
    ! @deploy-staging ship the current branch
    ```

    Up to five, and parsing stops at the first word that is not a skill - so an email
    address at the start of a message is left alone. An invoked skill arrives with its
    bundled `references/`, `scripts/` and `assets/` files *named* (not read), so the
    assistant knows what is there without spending a turn looking.

    A project's skills arrive with a `git clone`, so treat them as you would any other
    file in that repository. They are notes, never permission: a skill cannot authorise
    skipping a confirmation, and a script bundled with one always asks before it runs -
    including under an unattended task, where `--allow-command` does not cover it, and
    including one under `.agents/skills/`. `AI_CHAT_PROJECT_SKILLS=0` turns both
    project roots off entirely.

    This repository keeps canonical sample skills under `docs/ai/skills/`; install the
    ones you want with `scripts/install-runtime-skills.sh --target dogesh --profile dogesh-common`.
    One of them, `dsh-cron`, teaches the `!` agent to add, edit and debug your own
    [cron jobs](usage.md#cron-jobs) — install it on its own with `--profile dogesh-user`.

14. **AI chat hooks**:
    Your own checks, run around the `!` agent loop. Distinct from the Lisp
    [hook system](lisp-functions.md#hook-system-functions), which fires around ordinary command execution.

    Put them in `~/.config/dogesh/ai-hooks.json` (`DOGESH_AI_HOOKS_CONFIG` points elsewhere):

    ```json
    {
      "version": 1,
      "hooks": [
        {
          "id": "block-etc-writes",
          "events": ["pre-tool-use"],
          "match": {
            "tools": ["edit", "str_replace", "execute"],
            "paths": ["/etc/**"]
          },
          "command": ["python3", "/home/me/.config/dogesh/hooks/block-etc.py"],
          "timeout_ms": 3000
        }
      ]
    }
    ```

    | Event | When | Can it stop anything? |
    |---|---|---|
    | `session-start` | a new conversation begins | no |
    | `user-prompt-submit` | before your message is sent | yes |
    | `pre-tool-use` | before any tool runs, MCP included | yes |
    | `post-tool-use` | after a tool returns | it can mark the result failed |
    | `pre-compact` | before the conversation is shortened | no |
    | `response-complete` | after the answer | no |

    `pre-compact` fires when the conversation has grown past
    `AI_CHAT_CONTEXT_TOKEN_BUDGET` (or the internal buffer limit), after the free
    rule-based pass and before dogesh pays a model to summarize - its
    `will_summarize` says which. It cannot refuse: without the shortening the
    request would simply be too large to send.

    **Which calls a hook wants** is the `match` clause. dogesh puts every command
    through the one `execute` tool, so `{"tools": ["execute"]}` means *every
    command* and a hook watching `rm` would pay its timeout on every `ls`. The
    other three kinds narrow that. Kinds are ANDed; entries within a kind are ORed.

    | Key | Matches |
    |---|---|
    | `tools` | tool names; `*` allowed only as the last character (`mcp__*`) |
    | `programs` | programs the command runs, written the way `AI_CHAT_EXECUTE_ALLOWLIST` is - `execute` only |
    | `paths` | globs against the paths the call names |
    | `arguments` | exact values for top-level argument fields |

    `programs` looks through wrappers and judges every stage, so `rm` matches
    `sudo rm -rf x`, `timeout 5 rm x` and `echo hi \| rm -rf x` alike, and
    `git push` matches `git push --force` but not `git pushx`. `paths` sees the
    `path` argument of `edit`/`read_file`/`str_replace`/`ls`/`search`, every
    non-option token and the `cwd` of an `execute` command, and any path-shaped
    string anywhere in an MCP tool's arguments (nested objects and lists
    included) - each both as written and resolved against the directory the call
    will run in, so `/etc/**` matches `sub/../../etc/shadow` and matches
    `{"command": "cat sub/x", "cwd": "/etc"}`. Inside a command line a bare word
    could be a file or a program and the two cannot be told apart, so both are
    offered: `rm .env` reaches a hook watching `**/*.env`, and the price is that
    a deliberately broad pattern like `<cwd>/**` is true of a program name too.
    `skill_manage` contributes no paths - its `file` is relative to the skill,
    not to your working directory - so watch skill writes with
    `"tools": ["skill_manage"]` instead. Resolution is textual, never `realpath`, so a symlink
    can dodge a `paths` matcher; that costs you a check that did not run, never a
    permission, because a hook cannot grant anything.

    A `match` that could never be true is a load error - `programs` without an
    `execute` entry in `tools`, an unparseable program, a bad glob, a non-scalar
    `arguments` value - because a hook that loads cleanly and never fires is the
    worst outcome available. Where a matcher is merely *pointless* rather than
    impossible, `doctor hooks` says so and the hook still loads: an empty
    `match` runs on every call, as it always has, and a matcher that cannot be
    satisfied on only *some* of a hook's events still works on the rest.

    The match is made against the arguments as the model sent them, while the
    payload the hook receives is masked. They differ on purpose: masking rewrites
    `-p <value>` unconditionally, so `mkdir -p /etc/myapp` reaches a hook as
    `mkdir -p ***`, and a `/etc/**` matcher reading that would not fire. An
    argument list that is present but cannot be read - a mismatched quote -
    counts as a match, so a broken command line is never a way past a check; a
    call with no arguments at all simply does not satisfy a matcher about them.

    The event arrives as JSON on the hook's **stdin**; the hook answers with JSON on
    stdout, or just exits:

    ```json
    { "decision": "deny", "reason": "writes outside the repo are not allowed here",
      "message": "shown to you, not to the model",
      "additional_context": "added to what the model reads" }
    ```

    Every event carries where the agent loop is, so a hook can govern the run and
    not just the call - stop a tool once the loop has gone round forty times, or
    once the turn has spent more than you meant to:

    ```json
    { "hook_version": 1, "event": "pre-tool-use", "session_id": "...", "turn_id": "...",
      "cwd": "/repo", "safety_level": "normal", "agent_task_id": null,
      "tool_name": "execute", "tool_arguments": { "command": "cargo test" },
      "loop": { "iteration": 12, "max_iterations": 100,
                "tokens": { "prompt": 88000, "completion": 4200, "total": 92200 },
                "turn_token_budget": null },
      "hook_budget": { "turn_budget_ms": null, "spent_ms": 40 } }
    ```

    Fields are only ever added to the payload, so `hook_version` stays `1`; it
    moves only if a key changes meaning or goes away.

    A short hook should not need a JSON parser, so the same facts arrive as
    environment variables: `DOGESH_HOOK_EVENT`, `DOGESH_HOOK_ID`, `DOGESH_HOOK_SESSION_ID`,
    `DOGESH_HOOK_TURN_ID`, `DOGESH_HOOK_CWD`, `DOGESH_HOOK_TOOL`, `DOGESH_HOOK_ITERATION`,
    `DOGESH_HOOK_MAX_ITERATIONS`, `DOGESH_HOOK_TURN_TOKENS` and `DOGESH_HOOK_TIMEOUT_MS`
    (the timeout that actually applies). `DOGESH_HOOK_DEPTH` is set too, and is what
    stops a hook that starts `dogesh` from running hooks of its own.

    `additional_context` lands next to the thing it is about, on the three events
    that have somewhere to put it: `user-prompt-submit` (beside your message),
    `pre-tool-use` (before the tool result) and `post-tool-use` (after it). The other
    two say so in a warning rather than dropping it silently.

    `decision` is `"deny"`, `"ask"` or absent. **There is no `"allow"`**: a hook can stop
    something or insist you are asked about it, never permit it - the safety guard stays
    the only thing that grants anything. Exiting `2` is shorthand for `deny` with the
    first line of stderr as the reason; exiting `0` with no JSON means "carry on".

    `command` is an argument vector, not a shell line: the hook is executed directly, so
    put any pipeline in a script file. The first element must be an **absolute path** or
    a bare name found on `PATH` - a relative one like `./hook.sh` is refused, because it
    would resolve against whatever directory the chat happens to be in when the hook
    runs, which is the very thing that keeps `.dogesh/hooks.json` unread. A bare name is
    looked up once, at load time.

    The configuration file (and its directory) must not be world-writable; it is a list
    of commands the shell will run for you. Group-writable is fine, so a `umask` of 002
    needs no special handling. Hooks run as you, with your environment - read one
    before installing it, the way you would read a `config.lisp`. A hook that fails or
    times out on `user-prompt-submit` or `pre-tool-use` **blocks**, because a check that
    can be got past by crashing is not a check; failures on the other three events are
    reported once and ignored. `AI_CHAT_HOOKS=off` disables the lot - budget included, so a
    typo there cannot outlive the switch - and `doctor hooks` shows what was
    loaded without running anything.

    Each hook has its own `timeout_ms` (5000 by default, clamped to 100-60000), and at
    most eight hooks run for one event. `AI_CHAT_HOOK_TURN_BUDGET_MS`
    puts a ceiling on the wall time one turn spends waiting for all of them. It is off
    by default, because a default would mean hooks quietly stopping partway through a
    long turn. When the budget runs out, `post-tool-use`, `pre-compact`,
    `session-start` and `response-complete` are **skipped** with one warning per
    turn, while
    `user-prompt-submit` and `pre-tool-use` still run on whatever is left (at least
    100ms) - a check that could be got past by being slow would not be a check, and a
    gate that times out already refuses. Eight hooks each need their 100ms, so the
    budget bounds the wait rather than guaranteeing it. Time spent answering a hook's
    own question is not charged to it.

    Hooks are synchronous, but an observer need not be. Print your line, exit `0`, and
    leave the slow part to a child of your own: only a timeout or a `Ctrl-C` kills the
    process group, so a hook that has already exited leaves its background work alone.

    An unknown event name, an unknown `match` field or an unknown key anywhere in the
    file is a load error, and a load error refuses the chat rather than running without
    the checks. That also means a configuration using the newer `match` kinds will not
    load on an older `dogesh` - deliberately, and for the same reason: a typo must not be
    a way for a gate to disappear quietly.

    Project-local `.dogesh/hooks.json` is deliberately **not** read: cloning a repository
    should not be enough to run its commands.

For maintainers, concise AI/Skill authoring notes live in `docs/ai/README.md`.


