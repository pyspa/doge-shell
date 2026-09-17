---
name: dsh-chat
description: Use when using the doge-shell `!` AI chat itself - starting a chat, picking a model, @mentioning skills, MCP tools, approvals, sessions, `!` チャットの使い方, モデル変更, チャットが動かない. Covers daily chat use, not repo development.
---

# DSH Chat

- Start with `! <request>`. An API key alone runs nothing - AI features stay off until used explicitly.
- The provider is OpenAI-compatible `chat/completions` only. Show the model with `chat_model`, change it with `chat_model <name>`.
- Mention a skill with a leading `@name` (up to 5). `skill list` shows what the runtime reads.
- MCP tools: `mcp status` first, then `mcp tools`.
- File writes and destructive commands ask first. Answer the prompt; an unattended task stalls instead - see $dsh-agent.
- `chat_status` inspects the carried conversation; `chat_reset` forgets it.
- Diagnose in this order: `doctor skills` -> `doctor ai`.
- Setup and keys: [references/setup.md](references/setup.md).
- Skills, mentions and project trust: [references/skills-mentions.md](references/skills-mentions.md).
- Symptom-to-cause table: [references/troubleshooting.md](references/troubleshooting.md).
