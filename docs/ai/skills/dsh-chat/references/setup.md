# Chat setup

- The provider speaks OpenAI-compatible `chat/completions` at `{base_url}/chat/completions` with a bearer key. An Anthropic Messages API endpoint does not work directly - use an OpenAI-compatible gateway.
- API key, model, base URL and timeout resolve in one place. Set them once and do not re-configure them per command.
- Show the current model with `chat_model`; change it with `chat_model <name>`. The change takes effect immediately, everywhere.
- Set a session-local system prompt with `chat_prompt <text>` when the task needs a standing instruction.
- Nothing automatic runs on a key alone: inline suggestions, auto-fix and explanations default off. `!`, `safe-run` and `ai-watch` are explicit operations and are unaffected by those defaults.
- When the chat cannot reach the provider at all, run `doctor ai` and follow its output before changing configuration.
