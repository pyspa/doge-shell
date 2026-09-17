# Troubleshooting: symptom to cause

Work through `cron status`, then the relevant row below, before guessing. Re-running
`cron run NAME --now` after each change is the fastest way for a **person** to confirm a
fix - it claims and executes the job immediately and in the foreground, bypassing the
schedule. `cron_manage`'s `run` action cannot do this (it only marks the job due for the
next tick, same as `cron run NAME` with no `--now`) - from a chat tool, ask a person to
run `--now` and report back, or use `cron history`/`cron logs` to see the outcome of
whatever the job's own schedule or the next tick already produced.

| Symptom | Likely cause | Check |
|---|---|---|
| Job never fires at all | No session open and no external tick installed | `cron status` shows overdue jobs and that no run has completed. Confirm tick arrival in the scheduler logs or with a verbose tick run, then install one: [external-tick.md](external-tick.md) |
| Job fires late, not on time | The session runner scans on a schedule capped at 60 seconds when idle | Expected for a slow-interval or cron-expression job; only matters for a sub-minute interval job with nothing else due |
| `cron run NAME` (no `--now`) seems to do nothing for a while | It only marks the job due; the session runner or next external tick picks it up, up to ~60s later. It also refuses outright on a paused or blocked job (nothing would ever pick it up) | A person can use `cron run NAME --now` for an instant result instead; `cron resume NAME` first if it was paused |
| Job fires twice for what looks like one moment | Actually two different scheduled slots close together, or a DST overlap that resolved to the earlier instant on purpose | `cron history NAME` - the store refuses two runs for the same slot, so distinct `when`/`run` rows are distinct slots |
| Command works when typed at the prompt, fails only from cron | cron jobs run under a plain `sh -c`, not an interactive shell: aliases, abbreviations, shell functions and Lisp functions are not available; `PATH` and other exports are whatever was in effect *when the job was registered*, not the shell's current state | Write the full command, or call a script; re-`cron add --force` (or `cron rm` and re-add) after changing exports so the job picks up the new snapshot - `cron edit` has no `--env` and cannot update it |
| Command fails only when run via the external tick, not interactively | The tick runs outside any login/interactive shell, so `PATH` may be shorter and env files may not be sourced | Use absolute paths in the command, or a wrapper script that sets up its own environment |
| A shell job's exit code is always the timeout code | The command outlived `--timeout` and was killed | `cron edit NAME --timeout <longer>`, or fix the command |
| Desktop/above-prompt notification never appears for a finished run | Per-run REPL notices are not wired for cron yet - a run finishes in its own separate process, not the session that is watching the prompt | Check the outcome with `cron history` (and the run's recorded output with `cron logs NAME` - each stream clamped to 8 KiB at record time) instead; this is a known current gap, not a misconfiguration. `cron doctor` reports it as a `note` whenever any job's `--on` is not `never` |
| A job has been `running` for a suspiciously long time | Either it is a genuinely slow run, or its process died without releasing the claim | `cron doctor` flags any currently-`running` job with how long it has held its claim; if that keeps growing across repeated `cron doctor` calls, wait for the lease to expire (see the row above) |
| `cron history` shows nothing at all for a job that should have run | The claim never succeeded - check for a stuck lease from a crashed process | Wait for the lease to expire (twice the job's `--timeout`, minimum 60s) and it self-recovers on the next scan; `cron status` will show it as no longer running once reclaimed |
| Editing a job seems to do nothing | `cron edit` only touches the fields you name; a bare `cron edit NAME` with no flags is refused rather than silently no-opping | Re-run with the specific `--schedule`/`--command`/`--on`/... flag for what you meant to change |
| `cron add` says the name already exists | `add` refuses to silently replace a job - a second run of the same command is far more likely to be a typo than an intentional re-declaration | Use `cron edit` to change one field, or `cron add --force` if you really mean to replace it. (`config.lisp`'s `(cron-add ...)` is the exception: it upserts by design, since it runs on every launch.) |
