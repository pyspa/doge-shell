# Grants and budgets

Grant-shaped options (`--read` / `--write` / `--allow-command` /
`--allow-mcp` / `--network` / `--env`) are shared with `cron add --agent`,
so an unattended job's grant validates exactly the way an interactive one
does (`dsh/src/agent.rs`). Nothing beyond what is granted at creation time
is available at run time. A missing grant no longer stops the task at the
first refusal: the denial is returned as a tool error the task routes
around, and only a task that cannot proceed stops - `interrupted` with the
refusal hint (resumable with wider grants), or `input-required` after the
same refused operation three times. `agent approve ID` reads that same
refusal, asks once, widens the grant by that one refusal, and resumes in
the foreground (`--dry-run` previews without changing anything).

## Grants

- `--read DIR` / `--write DIR` (repeatable): directories the task may read /
  modify. The task starts with the shell's current directory readable.
- `--allow-command EXACT` (repeatable): the exact command line the task may
  run. It must match what the task needs byte-for-byte - guess the shape
  from `agent show` / incident output rather than inventing it.
- `--allow-mcp ENTRY` (repeatable): the exact MCP approval key. Copy it from
  plain `agent show ID` (the full record) - the `--summary` report does not
  carry it.
- `--network HOST` (repeatable): sandbox hosts the task may reach.
- `--env NAME` (repeatable): environment variable *names* the task may read
  at run time, never values. Granting a secret's name does not expose its
  value on the command line; putting the value itself there does.
- `--sandbox` (`run`-only): require pinned SRT isolation.

## Time budget

- `agent run [--timeout SECONDS]` sets the cumulative time budget in seconds.
  `agent resume` accepts the same flag to raise it on an existing task.
- Without the flag the value falls back to `AI_AGENT_TIMEOUT_SECS`, read as
  a shell variable first and the process environment second, and finally to
  the built-in default (1800s). `cron add --agent` starts from the same
  default. Token usage is recorded and shown but never caps a run.
- A final round that lands exactly on its time budget with verified
  work done still counts as completed.
- `AI_AGENT_MAX_CONCURRENT` (default 1) bounds how many tasks - detached or
  not - may run at once.

## Completion criteria

- `--check TEXT` (repeatable, `run`-only) names one completion criterion.
  Criteria can only be set once, before work starts; the task records a plan
  first and verifies each criterion against a successful `tool_result`
  event. A failed test, a pending job, or a result that predates the latest
  action is not valid evidence.
- Write each `--check` as something the task can point at concrete evidence
  for (a file containing a line, a command exiting zero), not a subjective
  judgement call.
