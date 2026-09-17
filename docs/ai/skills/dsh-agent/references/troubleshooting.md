# Troubleshooting: symptom to cause

Read the stop reason first: `agent show ID --summary`. The full event
history behind it is plain `agent show ID`.

| Symptom | Likely cause | Check |
|---|---|---|
| Task sits in approval wait (`InputRequired`) | The same refused operation was tried three times, or an operation outcome stayed unknown | `agent show ID --summary` for the repeated call and its result; fix the cause, then `agent resume ID --reconcile TEXT` |
| Task stopped `interrupted` with a `needs:` line | It worked around grant refusals until it could not proceed | The `needs:` line names the exact resume command (`--allow-command` / `--allow-mcp` / `--write`); a single refusal no longer stops the task, it is returned as a tool error the task routes around. Or run `agent approve ID` to confirm the one grant and resume in one step |
| Task waits on a remote (MCP) task (`InputRequired`, `agent respond` needed) | The provider parked its own task on user input | `agent show ID` for the server and remote task ID, then `agent respond ID SERVER REMOTE_TASK_ID JSON` and `agent resume ID` |
| Task stopped `interrupted` with a `needs:` line | It worked around grant refusals until it could not proceed | The `needs:` line names the exact resume command (`--allow-command` / `--allow-mcp` / `--write`); a single refusal no longer stops the task, it is returned as a tool error the task routes around |
| Budget exhausted before the work is done | `--tokens` / `--timeout` too small | `agent resume ID --tokens N --timeout SECONDS` with larger values |
| `another agent task is active` | `AI_AGENT_MAX_CONCURRENT` (default 1) is reached | `agent list` for the holder; `agent wait` / `agent cancel` it, or raise the limit |
| `task <id> is already running` on resume/delete | The task is still going | `agent wait ID`, or `agent cancel ID` first |
| `nested agent invocation is not allowed` | `agent` was typed inside a task's own shell | You are inside the task - hand the CLI command to the person outside instead |
| `reconcile the previous operation before ...` | The previous run stopped mid-operation | `agent resume ID --reconcile TEXT` with what was actually observed |
| Missing `--allow-mcp` key, exact shape unknown | The key must match byte-for-byte | Plain `agent show ID` (full JSON), not `--summary` - the summary does not carry the key |
| Task was killed by its own watchdog | It outlived `--timeout` | `agent show ID` still holds the full record; re-run or resume with a larger `--timeout` |
| Task fails immediately on every run | Missing API key or unreachable provider | A scheduled job settles these as a `config` incident rather than retrying (see $dsh-cron); an interactive run reports the error directly - `agent doctor` covers neither, it reports stuck tasks, stale state, and missing default budgets |
