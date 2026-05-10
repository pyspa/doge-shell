---
name: doge-shell-serve-web
description: Use for doge-shell serve, static files, HTTP handlers, CORS, scanner, path validation, or web serving work.
---

# Doge Shell Serve Web

- Start with `rg -n "serve|handler|scanner|static|CORS|path|traversal" dsh-builtin/src/serve dsh-builtin/src/mcp.rs dsh-types/src`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for serve / MCP server entries.
- Read [references/checklist.md](references/checklist.md) before changing request routing, filesystem access, or response shape.
- Keep web serving changes in `dsh-builtin/src/serve/` unless shared MCP types or command wiring prove involved.
- Validate with `cargo test -p dsh-builtin`; add `cargo test -p dsh-types` only when shared types change.
