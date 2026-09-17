# Troubleshooting: symptom to cause

Read the stop reason first: `agent show ID --summary`. The full event
history behind it is plain `agent show ID`.

| Symptom | Likely cause | Check |
|---|---|---|
| Task sits in approval wait (`InputRequired`) | It needs a grant it was not given | The stop reason names the missing permission; re-`run` with the grant added, or `agent resume ID --reconcile TEXT` once the grant side is settled |
| Same operation failed three times | The task now waits for a person on purpose | `agent show ID --summary` for the repeated call and its result; fix the cause, then `agent resume ID --reconcile TEXT` |
| Budget exhausted before the work is done | `--tokens` / `--timeout` too small | `agent resume ID --tokens N --timeout SECONDS` with larger values |
| `another agent task is active` | `AI_AGENT_MAX_CONCURRENT` (default 1) is reached | `agent list` for the holder; `agent wait` / `agent cancel` it, or raise the limit |
| `task <id> is already running` on resume/delete | The task is still going | `agent wait ID`, or `agent cancel ID` first |
| `nested agent invocation is not allowed` | `agent` was typed inside a task's own shell | You are inside the task - hand the CLI command to the person outside instead |
| `reconcile the previous operation before ...` | The previous run stopped mid-operation | `agent resume ID --reconcile TEXT` with what was actually observed |
| Missing `--allow-mcp` key, exact shape unknown | The key must match byte-for-byte | Plain `agent show ID` (full JSON), not `--summary` - the summary does not carry the key |
| Task was killed by its own watchdog | It outlived `--timeout` | `agent show ID` still holds the full record; re-run or resume with a larger `--timeout` |
| Task fails immediately on every run | Missing API key or unreachable provider | A scheduled job settles these as a `config` incident rather than retrying (see $dsh-cron); an interactive run reports the error directly - `agent doctor` covers neither, it reports stuck tasks, stale state, and missing default budgets |
