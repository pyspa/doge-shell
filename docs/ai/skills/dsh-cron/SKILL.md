---
name: dsh-cron
description: Use when adding, editing or debugging a doge-shell cron job - cron add/edit/list/history/tick, shell or AI agent jobs, crontab/systemd timer/launchd ticks, 定期実行, ジョブ追加, スケジュール, cron が動かない. Covers schedule syntax and unattended runs.
---

# DSH Cron

- Run `cron status` first. It says whether an external tick is arriving; without one, jobs only fire while a dsh session is open.
- Add with `cron add --name NAME '<schedule>' <command...>`. Quote the schedule - an unquoted five-field expression is glob-expanded by the shell before dsh sees it.
- Add an AI job with `--agent`: `cron add --agent --name NAME --tokens N --timeout 10m --write . --check '...' '<schedule>' -- '<goal>'`. Grants work exactly like `agent run`; nothing is asked at tick time, so a missing grant stalls the run instead of prompting.
- Create new jobs `--paused`, run `cron run NAME --now` once to see the real output, then `cron resume NAME`.
- Edit one field at a time: `cron edit NAME --schedule '...' | --command '...' | --on failure | --timeout 2m`. Re-adding the same `--name` with `cron add` needs `--force`; it never silently creates a duplicate.
- Debug in this order: `cron status` -> `cron doctor` -> `cron history NAME --failed` -> `cron run NAME --now`. For an AI job continue with `agent show <task-id>` from the history row.
- An unattended AI job that needs an approval it was not granted shows up in `cron incidents`, not as a prompt. Add the grant with `cron edit`, then `cron incidents ack <id>`.
- From inside a `!` chat or an `agent run` task, use the `cron_manage` tool, not `execute` - `cron` is a builtin, not a shell command, so `execute("cron ...")` cannot reach it. A job the tool creates always starts paused, and its grant can never exceed the calling task's own; see [references/ai-jobs.md](references/ai-jobs.md#the-agent-creating-and-managing-its-own-jobs).
- Schedule grammar, intervals, `@daily`, and timezone behaviour: [references/schedule-syntax.md](references/schedule-syntax.md).
- Installing the external tick on Linux and macOS: [references/external-tick.md](references/external-tick.md).
- Symptom-to-cause table for a job that never fires, fires twice, or fails only from cron: [references/troubleshooting.md](references/troubleshooting.md).
- Budgets, grants and completion criteria for unattended AI jobs, plus the per-job notepad: [references/ai-jobs.md](references/ai-jobs.md).
- Commands run under `sh -c` from the job's recorded `--cwd`; aliases, abbreviations, builtins and Lisp functions are not available inside them. Write the full command or call a script.
- Never put a secret on a job's command line - `cron list` and `cron history` show it. Use `--env NAME` on an agent job, or read the secret inside the script.
- `list`/`show`/`history`/`incidents`/`status`/`doctor` all take `--json` (`notepad` does not - it prints the raw text).
- Removing a job or acknowledging an incident is a real decision - confirm with the person before running `cron rm` or `cron incidents ack`.
