---
name: dsh-agent
description: Use when running or following a doge-shell persistent agent task - agent run/resume/list/logs/wait/show/cancel/doctor, detached tasks, budgets and grants, 永続タスク, 承認待ち, agent が終わらない. Covers CLI use and the `!` chat boundary.
---

# DSH Agent

- Run `agent list` first to see live tasks; `agent doctor` when something looks stuck.
- Start with `agent run --tokens N --timeout SECONDS [--check TEXT] [--write DIR]... -- GOAL`. The `--` before the goal is required. `--check` is repeatable and each criterion is verified against a recorded tool result, so write it as something checkable, not a judgement call.
- Grants (`--read`, `--write`, `--allow-command`, `--allow-mcp`, `--network`, `--env`, `--sandbox`) are fixed at creation; a missing grant stalls the task instead of prompting. Same flags and validation as `cron add --agent`.
- Budgets come from flags, or from `AI_AGENT_TOKEN_BUDGET` / `AI_AGENT_TIMEOUT_SECS` (shell variable, then environment). The token budget stops subsequent requests, not a billing cap. `AI_AGENT_MAX_CONCURRENT` (default 1) bounds how many tasks may run at once.
- Detach long runs: `agent run ... --detach -- GOAL`, then follow with `agent list` / `agent logs ID [--follow]` / `agent wait ID`. Inspect with `agent show ID --summary`; plain `agent show ID` is the full unabridged record.
- A stalled task: `agent show ID --summary` for the stop reason. Approval wait means adding the missing grant and `agent resume ID --reconcile TEXT`; budget exhaustion means `agent resume ID --tokens N --timeout SECONDS`.
- From inside a `!` chat or an `agent run` task, do not call `execute("agent ...")` - `agent` is a builtin, unreachable from `execute`, and there is no `agent_manage` tool. Tell the person the CLI command instead; a nested `agent` invocation inside a task is refused outright.
- Never put a secret on the command line - `agent list` / `agent show` display it. Use `--env NAME` to grant a name (never a value).
- `list` / `logs` / `doctor` take `--json`; `show` takes `--summary`. `cancel` / `delete` throw away recorded work - confirm with the person first.
- Scheduled runs are cron's job, not this command's: use `cron add --agent ...` instead - see $dsh-cron and [its AI-jobs reference](../dsh-cron/references/ai-jobs.md).
- Deciding between `!` and `agent run` from the interactive side is $dsh-chat territory.
- Command shapes, detach and `respond`: [references/run-resume.md](references/run-resume.md).
- Grants and budgets in detail: [references/grants-budgets.md](references/grants-budgets.md).
- Symptom-to-cause table: [references/troubleshooting.md](references/troubleshooting.md).
