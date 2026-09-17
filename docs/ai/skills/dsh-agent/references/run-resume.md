# Run, resume, and follow a task

All shapes below are the `HELP` text `agent help` prints (`dsh/src/agent.rs`).
Quote values the shell would otherwise split or glob-expand.

## Start

```sh
agent run [--timeout SECONDS] [--check TEXT]... \
  [--read DIR]... [--write DIR]... [--allow-command EXACT]... \
  [--allow-mcp ENTRY]... [--network HOST]... [--env NAME]... \
  [--sandbox] [--detach|-d] -- GOAL
```

- `--` before the goal is required; everything after it is joined into the goal text.
- `--timeout` falls back to `AI_AGENT_TIMEOUT_SECS` (shell variable, then
  environment), then to 1800s.
- `--check` is repeatable but only accepted on `run`: criteria are fixed once,
  before work starts, and each one must later be verified against a recorded
  tool result. `--sandbox` is also `run`-only. `--detach` (`-d`) works on both
  `run` and `resume`.
- A fresh task starts in the shell's current directory, which is also its
  initial readable directory.
- `agent run` without `--detach` blocks until the task reaches a stopping
  point; a non-success exits nonzero with `task <id> stopped; inspect with
  agent show`.

## Resume

```sh
agent resume ID [--timeout SECONDS] [--reconcile TEXT] [--detach|-d]
agent approve ID [--reconcile TEXT] [--dry-run]
```

- `--timeout` raises the cumulative time budget of the existing task.
- `--reconcile` reports the observed result of the interrupted operation the
  task stopped on; without it a task waiting on `pending_operation` refuses
  to continue.
- `approve` asks once about the one refusal the task is stuck on, widens
  the grant by exactly that refusal, and resumes in the foreground
  (one refusal per run; `--dry-run` previews). Refusals no flag can
  satisfy are reported with manual steps instead. Resuming a detached
  continuation stays `resume --detach`'s job.
- Resuming a task that is currently running is refused - `agent wait ID` or
  `agent cancel ID` first.

## Follow without blocking

```sh
agent list [--all] [--json]
agent logs ID [--follow] [--json]
agent wait ID [--timeout SECONDS]
agent show ID [--summary]
agent cancel ID
agent delete ID
agent doctor [--json]
```

- `list` hides finished tasks older than a day; `--all` shows everything. A
  leading `*` marks a task that is still going.
- `logs --follow` / `wait` poll until the task stops; `wait --timeout`
  bounds how long the watcher itself waits.
- `show --summary` prints the human-readable report (goal, criteria with
  evidence, token usage, time use, tool-call counts). Plain `show` is the full JSON
  dump of the task plus every event - the only place an exact `--allow-mcp`
  approval key can be copied from byte-for-byte.
- `delete` refuses a currently-running task; `cancel` is the way to stop one.
- `respond` delivers input to a remote (MCP) task and is CLI-only - there is
  no chat-tool equivalent:
  `agent respond ID SERVER REMOTE_TASK_ID JSON_INPUT_RESPONSES`, then
  `agent resume ID` to continue polling.
- `run-detached` is internal (started by `--detach` itself) - never call it
  directly.
