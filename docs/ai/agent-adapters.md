# Coding agent adapters

| Capability | Portable contract | Codex | Claude Code | OpenCode |
|---|---|---|---|---|
| Repository rules | `AGENTS.md` | native | `CLAUDE.md` imports | native |
| Project Skills | core `.agents/skills` | native | core `.claude/skills` | both roots |
| Domain guidance | router `skills[].path` | direct read | direct read | direct read |
| Checks | repository scripts and tests | same | hook calls fast check | same |
| Noninteractive evaluation | `AgentRunner` | JSON events | headless, version dependent | JSON events |
| Structured trace | optional | available | probed | available |
| LSP | optional | capability dependent | Code Intelligence | optional |
| Independent review | review packet and schema | fresh session | fresh session or subagent | fresh session or subagent |

Agent-specific hooks and trace parsing provide early feedback or optional metrics. CI, checkers, tests, and normalized evaluation results provide the shared contract.
