---
name: dsh-cron
description: Use when adding, editing or debugging a doge-shell cron job - cron add/edit/list/history/tick, crontab/systemd timer/launchd ticks, 定期実行, ジョブ追加, スケジュール, cron が動かない. Covers schedule syntax and scheduled runs.
---

# DSH Cron

- Run `cron status` first. It says whether an external tick is arriving; without one, jobs only fire while a dogesh session is open.
- Add with `cron add --name NAME '<schedule>' <command...>`. Quote the schedule - an unquoted five-field expression is glob-expanded by the shell before dogesh sees it.
- Create new jobs `--paused`. `cron run NAME --now` (CLI only - `cron_manage`'s `run` cannot run synchronously) runs it right away to see the real output before `cron resume NAME`; from a chat tool, ask a person to run that once instead.
- Edit one field at a time: `cron edit NAME --schedule '...' | --command '...' | --on failure | --timeout 2m`. Re-adding the same `--name` with `cron add` needs `--force`; it never silently creates a duplicate.
- Debug in this order: `cron status` -> `cron doctor` -> `cron history NAME --failed` -> `cron logs NAME`. `cron logs NAME` shows a run's full recorded stdout/stderr - use it, not `history`'s one-line preview, to read what a past run produced.
- From inside a `!` chat, use the `cron_manage` tool, not `execute` - `cron` is a builtin, not a shell command, so `execute("cron ...")` cannot reach it. A job the tool creates always starts paused.
- Schedule grammar, intervals, `@daily`, and timezone behaviour: [references/schedule-syntax.md](references/schedule-syntax.md).
- Installing the external tick on Linux and macOS: [references/external-tick.md](references/external-tick.md).
- Symptom-to-cause table for a job that never fires, fires twice, or fails only from cron: [references/troubleshooting.md](references/troubleshooting.md).
- Commands run under `sh -c` from the job's recorded `--cwd`; aliases, abbreviations, builtins and Lisp functions are not available inside them. Write the full command or call a script.
- Never put a secret on a job's command line - `cron list` and `cron history` show it. Read the secret inside the script instead.
- `list`/`show`/`history`/`logs`/`incidents`/`status`/`doctor` all take `--json` (`notepad` does not - it prints the raw text).
- Removing a job or acknowledging an incident is a real decision - confirm with the person before running `cron rm` or `cron incidents ack`.
