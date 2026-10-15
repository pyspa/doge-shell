# Background AI tasks with `chat_task`

[← README](../README.md) · [AI setup and tools](ai-integration.md)

`chat_task` lets an AI task run while you continue using the **same interactive
`dogesh` shell**. Start with a small read-only task. The model still uses your
configured provider and may incur its usual inference charges.

## Interactive shortcut

In an interactive terminal, use `!&` to start a read-only task and keep typing:

```text
!& Read README.md and summarize the project. Do not run commands.
!& --edit Fix the typo in docs/example.md.
!&
```

Bare `!&` opens the current task menu: `o` shows retained output, `a` shows a
pending approval, `s` asks before stopping, and `q` closes the menu. Enter,
Escape and Ctrl-C also close it. **Closing keeps the task running.** Stop needs
an explicit `y`; its default is No. Alt+Enter on `!&` opens the same menu.

The approval screen shows the operation, targets and available before/after or
command preview. Press `y` to approve once, `n` to deny, or `q` to leave it
pending; no IDs need to be typed. A changed request, permission scope or target
invalidates the displayed approval. A clipped, secret-masked or otherwise
incomplete preview cannot be approved from the menu or `chat_task approve`;
denial remains available. Deny or close it
and request a smaller operation with reviewable contents. `status` exposes
`preview_complete`, `preview_truncated` and `preview_redacted`. Control and Unicode
format characters are shown as explicit escapes; literal backslashes are escaped
separately, so a special filename cannot impersonate its visible spelling.
Credentials remain masked, while approval always binds the original operation.
Ordinary command execution still has the isolation limits described below.

A space must follow `!&` before task text. Only a leading `--edit` selects
workspace editing; `--` ends option parsing (`!& -- --some literal text`).
Quotes, `$variables`, `$(commands)`, pipes and trailing `&` in task text are
literal text sent to the model; they do not run as shell syntax. Unknown leading
options or options without task text fail locally. `!&` requires terminal input
and output and cannot be used in `dogesh -c`, scripts or captured output.
Use the explicit `chat_task` commands below for those command interfaces.
The existing `!`, normal shell `&` and Alt+Enter on ordinary commands keep their
current behavior. Task profiles, budgets and lifetime are shared with `chat_task`.

## First task: read a project summary

First [configure an AI provider](ai-integration.md), start `dogesh`, and `cd` to
the project directory you want the task to read. For this example that directory
must contain `README.md`. In that shell, run:

```sh
chat_task start --profile read-only -- "Read README.md and summarize the project. Do not edit files or run commands."
chat_task status
chat_task output
```

`start` returns an ID such as `chat-12345678-1234-1234-1234-123456789abc`.
You can run ordinary foreground shell commands while the task works or waits
for approval. Run `status` again to inspect progress; `output` shows the retained
output at that moment, without following it continuously. There is no automatic
notification or daemon to check after closing the shell.

The directory at submission is the task's fixed workspace root. Changing the
foreground directory later does not move that root. Submit from the project
root if the task needs several subdirectories; there is no command to widen an
existing task's roots.

## Read `status` before approving anything

`chat_task status` prints JSON. The main fields are:

| Field | Meaning |
|---|---|
| `task_id` | Identity of this task; use it with an approval request ID |
| `state` | `running`, `input_required`, `completed`, `cancelled`, `timed_out`, or `budget_exhausted` |
| `profile` | `read_only` or `workspace_edit` (JSON uses underscores) |
| `approval` | Pending request details, or `null` |
| `ok` | Worker result when available; inspect this as well as `state` and output |
| `usage` | Elapsed time, parent tool requests, tokens and transferred output |
| `tokens_reported` | Whether the worker reported token usage; `false` is not a measured zero |

`completed` means the worker finished, not that every requested tool succeeded.
Inspect `ok` and `chat_task output`; a denied tool or provider error may explain
an incomplete result.

A pending request might look like this **illustrative excerpt**:

```json
{
  "task_id": "chat-12345678-1234-1234-1234-123456789abc",
  "state": "input_required",
  "approval": {
    "request_id": 1,
    "tool": "execute",
    "targets": ["/your/project"],
    "command_preview": "cargo test -p dsh-types",
    "execution_warning": "This command will run without OS sandbox isolation.",
    "preview_truncated": false
  }
}
```

Read `tool`, every target and the complete command before deciding. For a write,
inspect `before_preview`, `after_preview` and `write_bytes`. For project
instructions or skills, `tool` may be `project_guidance`; read the `guidance`
preview before granting access. That request can appear before the first model
call. Previews mask recognized secrets; a masked or truncated preview may still
be insufficient for you to approve the operation.

Copy **both IDs from your own current status**, replacing the sample task ID
and request number below. Choose one response:

```sh
chat_task approve chat-12345678-1234-1234-1234-123456789abc 1
```

Or refuse that operation:

```sh
chat_task deny chat-12345678-1234-1234-1234-123456789abc 1
```

Approval is for that exact operation once; it is not a permanent allowlist.
Denial lets the model receive a tool error and decide how to continue. To stop
the whole task, use `chat_task cancel` instead. Changed file contents, changed
permissions, cancellation, task expiry, worker loss, another task or a reused
request ID invalidate approval. If the response is stale, inspect fresh status;
do not repeatedly approve an old ID.

## Profiles and command execution

| Start profile | Automatic operations |
|---|---|
| `read-only` | Native file reads and directory listing within the fixed workspace |
| `workspace-edit` | Those reads plus native edits within that workspace; protected/sensitive targets still require handling |

Both profiles may ask permission for a shell command. **Manually approving
`execute` runs an ordinary command without an OS sandbox**: native-tool path
roots do not confine that command's filesystem or network access. Inspect what
you approve. `sandboxed-test` is currently unavailable and fails explicitly;
there is no automatic fallback to unrestricted execution.

For a small edit, start from a directory you intend to modify:

```sh
chat_task start --profile workspace-edit -- "Read README.md and write a short summary to task-summary.md. Use native file tools; do not run shell commands."
chat_task status
chat_task output
```

To narrow the active task's permissions, run:

```sh
chat_task restrict read-only
```

Restriction revokes pending grants and stops owned command jobs. It does not
undo edits already performed, and it does not add new workspace roots.
Project documents, skills and foreground always-allow settings cannot widen the
chosen profile. MCP, cron, shell history/search, skill mutation and arbitrary
hooks are unavailable in this initial task runner. Active AI hooks cause
submission to fail rather than silently skipping those checks.

## Environment, conversation and lifetime

- One interactive shell owns **one active AI task**. `status`, `output` and
  `cancel` take no task ID; approval responses require both IDs.
- Use the same shell throughout. `dogesh -c 'chat_task start ...'` exits its
  parent shell immediately, so it is not a way to keep a task running detached.
- The task gets a branch of the carried conversation. It does not consume or
  replace the foreground `!` conversation, and its progress is not merged back.
  The background conversation is memory-only; there is no restart/resume command.
- Provider settings, cwd and selected environment values are snapshotted at
  submission. Foreground setting changes are not imported mid-task. Skill
  metadata is captured when the worker starts, and instruction/resource bodies
  are read when needed; files can still change after submission.
- Command children receive selected **exported** environment variables. The
  defaults are `PATH`, `LANG`, `LC_ALL`, `TMPDIR`. Add a required name explicitly
  with `--env NAME` before `--`; this does not export an otherwise unexported
  variable. Provider credentials used by model transport do not automatically
  become command environment grants.
- Current fixed limits are 10 minutes, 100 parent tool requests, 64,000 turn
  tokens and 8 MiB of transferred output. Waiting for approval counts toward the
  time budget. Limits are checked at tool/response boundaries; a single provider
  response can exceed the remaining token budget.
- Output display retains at most a 256 KiB tail, with secret masking before
  trimming. This is a bounded diagnostic view, not a permanent transcript or a
  guarantee that every possible secret is recognized.

## Cancel, collect output and clean up

```sh
chat_task cancel
chat_task status
chat_task output
```

Cancellation revokes pending approvals, stops this task's command jobs and
reaps its worker. Previously written files remain; cancel is not rollback.
Read or copy the result you need before starting the next task: only the most
recent task is retained, and a new start replaces the previous output/status.
There is no `chat_task cleanup`, history listing or task daemon. Closing the
parent shell normally cleans up its worker and owned commands. A host crash or
SIGKILL cannot run that cleanup and does not guarantee child-process cleanup.

## AI task versus an AI-started command job

`chat_task` runs an entire AI conversation with its own workspace/profile and
approval state. A **command job** is one long-running `execute` operation that
the assistant follows through the model tools `job_status`, `job_output` and
`job_cancel`. Those are tools for the assistant, not `chat_task` subcommands.

A foreground `!` conversation can also start command jobs; those belong to its
chat session and are managed as described in
[Long-running commands in `!` chat](ai-integration.md#long-running-commands-in--chat).
`chat_reset` controls the carried foreground conversation/jobs;
`chat_task cancel` controls the separate background task. Use the relevant one.

For implementation details and the permission model, see
[Background task design](design/ai/background-tasks.md).
