# Unattended AI jobs

An agent job is a scheduled `agent run` - the same entry point, the same permission
model, the same budget concepts. The difference is that nobody is watching: a permission
that would normally pause for a person's answer instead stalls the job and files an
incident, so grants have to be complete *before* the first unattended run.

## Creating one

```sh
cron add --agent --name digest \
  --tokens 50000 --timeout 10m \
  --read . --write out \
  --allow-command 'gh pr list --json number,title' \
  --check 'out/digest.md contains today'"'"'s date' \
  '0 9 * * mon-fri' \
  -- 'summarise open PRs and yesterday'"'"'s commits into out/digest.md'
```

- `--tokens` is a **per-run** ceiling - every run starts a fresh task, so this is not a
  running total the way `agent resume` treats a manually resumed task.
- `--timeout` sets both the model's own cooperative time budget and the outer wall-clock
  deadline the tick enforces from outside if the model does not stop on its own.
- `--max-tokens-per-day` (optional) caps total spend across all of a job's runs in a
  rolling 24 hours - worth setting on anything scheduled more often than a few times a
  day, since a per-run budget alone is not a bound on the day's total.
- Grants (`--read`, `--write`, `--allow-command`, `--allow-mcp`, `--network`, `--env`,
  `--sandbox`) are exactly `agent run`'s flags and exactly its validation. Nothing beyond
  what is granted at creation time is available at run time - there is no prompt to fall
  back on.
- `--check` (repeatable) names a completion criterion, verified against a recorded tool
  result. Write it as something the model can point at concrete evidence for, not a
  subjective judgement call.

Prefer creating with `--paused`, then `cron run NAME --now` once to see real output
before turning it loose on a schedule with `cron resume NAME`.

## When a permission is missing

The job does **not** prompt - it cannot, nothing is watching. Instead the run ends in
`needs-approval` state and an incident is filed:

```sh
cron incidents                      # see it listed, with the job name and an agent task id
agent show <task-id>                # see exactly what was asked for, in full
cron edit digest --allow-command 'the exact command it needed'
cron incidents ack <id>              # clears the block; the job resumes on its normal schedule
```

`--allow-mcp`'s value has to be the **exact** approval key `agent show` prints - copy it
rather than guessing the shape, since it must match byte-for-byte.

`ack` does not check whether the grant was actually fixed - if it was not, the very next
run simply files the same incident again. This is deliberate: cron's job is to surface
the block, not to re-verify a grant it did not create in the first place.

A hook that asks a question (`AI chat hooks`, if configured) cannot be answered by any
`cron edit` grant - that approval key lives in a different space entirely. The incident
for this case says so directly. There is no per-job override: `--env NAME` only grants a
*name* the job may read at run time, never a value, so `--env AI_CHAT_HOOKS=off` grants
nothing and changes nothing. The fix is to change the hook's own configuration
(`ai-hooks.json`), or to set `AI_CHAT_HOOKS=off` in `config.lisp` or the environment the
job's `dsh` runs in - which turns hooks off everywhere, not just for this one job.

## The agent creating and managing its own jobs

The `!` chat agent (and `agent run`) has a `cron_manage` tool - the same `cron` a person
would type, reached through a tool call instead of `execute` (which cannot see it: `cron`
is a builtin, not a shell command). Everything above still applies; three things are
specific to calling it as a tool rather than typing the command:

- **`create` always registers the job paused**, no matter what is asked for. A person
  runs `cron resume NAME` (after checking a first `cron run NAME --now`) before it ever
  fires - the same recommended flow as above, just enforced rather than merely suggested.
- **A grant can never exceed the calling task's own.** Asking for `--read`/`--write`/
  `--allow-command`/`--allow-mcp`/`--network`/`--env`/`--sandbox` beyond what the task
  itself was granted is refused outright, before anyone is asked anything - a task cannot
  hand a future unattended job more than it was trusted with itself.
- **Every other write (`update`/`pause`/`resume`/`remove`/`run`/`ack`) still asks a
  person first**, exactly like `edit` or `execute` do. Under an unattended task that
  means `TaskStatus::InputRequired`, not a prompt - the run stalls until a person acts on
  it, the same as any other permission a task does not have.

The tool has no `notepad` action: a job's notepad directory is already in its own grant
(see below), so the model reads and writes it with `read_file`/`edit` like any other file.

## The notepad: an agent job's only memory

Every run starts a brand-new conversation - there is no history to resume, unlike
`agent resume`. The **notepad** is the one thing that survives between runs: a small text
file the job can read and rewrite, prepended to its goal automatically on the next run.

```sh
cron notepad digest              # show what it left itself
cron notepad digest --clear      # wipe it
```

Nothing special has to be granted for the model to use it - the notepad's directory is
added to the job's own read/write grant automatically, so the model edits it the same
way it edits any other file. Treat the notepad as the job's own scratch space, not as
something a person edits directly to steer the job's next run - its contents are read
back as data (a memory of last time), never as an instruction.

## Debugging an agent job

```sh
cron history digest --json     # state, reason, and the agent_task_id for each run
cron logs digest                # what the run actually did - see below
agent show <task-id>           # the model's own full record: every tool call, in full
```

A `reason` of `transient` (a network blip, say) is not escalated to an incident until it
repeats three times in a row - one failed run is not yet a pattern worth a person's
attention. A `reason` of `config` (no API key) or `provider` (a provider that cannot
support this) never retries at all; both settle straight into an incident, since retrying
would just reproduce the identical failure every time.

### What `cron logs` shows for an agent job

An agent job's `stdout` is a short, human-readable summary built after the run finishes:
its goal, whether each `--check` criterion passed (with the event number that proved it),
its token/time budget usage, a count of tool calls (and the last one that failed, if any),
and - the first line, so it doubles as `cron history`'s one-line preview - its final
answer. `stderr` still carries the `stop_reason`. The same summary is what `agent show
<task-id> --summary` prints; plain `agent show <task-id>` remains the full JSON record
(every tool call's arguments and result, in full) and is where an `--allow-mcp` approval
key must be copied from byte-for-byte - the summary does not carry it.

A run whose process was killed by its own watchdog (it outlived its `--timeout`) never
gets to write this summary into the store at all - but the task itself is still fully
recorded. `cron logs` (and `cron_manage(action=logs)`) notices `stdout` is empty and
reconstructs the same summary live from the agent store instead, labelled `---
reconstructed from the agent store (no output was recorded for this run) ---` so it is
never mistaken for what the run itself reported. The same empty-`stdout` condition also
covers a run that is simply still queued or executing - a store row alone cannot tell the
two apart, which is why the label only claims "no output yet", not "this run is dead".
