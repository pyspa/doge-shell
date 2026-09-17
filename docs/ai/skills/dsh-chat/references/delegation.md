# Delegating from `!` to an agent task

- Stay in `!` for quick questions, interactive file edits reviewed turn by turn, and anything needing back-and-forth with the person.
- Hand off to `agent run --detach -- GOAL` when the work is long, multi-step, or fine to run in the background while the person does something else. A detached task runs the same chat machinery in a separate process; completion, failure and approval waits surface above the prompt.
- Grants are fixed at creation and a missing one stalls instead of prompting, so grant everything the task needs upfront - same flags and validation as `cron add --agent`, see $dsh-agent.
- Scheduled or repeated runs are not detach's job - use `cron add --agent ...`, see $dsh-cron.
- Follow a detached task with `agent list` / `agent logs ID [--follow]` / `agent wait ID`.
