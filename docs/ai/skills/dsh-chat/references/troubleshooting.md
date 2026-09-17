# Troubleshooting

| Symptom | Likely cause |
|---|---|
| `!` answers without calling any tool | No MCP server connected, or the skill was not mentioned - check `mcp status` and `skill list` |
| Model or key error on the first `!` | Provider or key misconfigured - run `doctor ai` before editing config |
| A 400 error about `reasoning_effort` or an unknown field | Server refuses an optional field - the client retries once with a correction and remembers it; a repeated 400 needs `doctor ai` |
| The chat starts fresh instead of continuing | Session expired, or the prompt/language/MCP set changed - `chat_status` cannot see the latter, so compare with the previous turn |
| Leaving the project directory starts a fresh chat | Continuation follows the workspace root, not the exact directory - leaving the project always starts over |
| A cron job never seems to run | No session open and no external tick installed - see $dsh-cron |
| A skill the chat just wrote never appears | Its frontmatter failed the writer-side lint (missing `name`/`description`) - run `doctor skills` to see the diagnostic |
