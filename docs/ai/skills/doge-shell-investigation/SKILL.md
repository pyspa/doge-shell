---
name: doge-shell-investigation
description: Use for investigation, review, audit, performance inspection, 調査, レビュー, 監査, or 性能確認 in doge-shell. Keeps work read-only first, avoids broad tests early, and narrows likely files before editing.
---

# Doge Shell Investigation

- Start with `rg --files` or `rg -n`; do not edit or run broad tests first.
- Read [../doge-shell-repo/references/read-boundaries.md](../doge-shell-repo/references/read-boundaries.md) before opening `README.md` or running workspace-wide commands.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) to narrow candidate files.
- Use [../doge-shell-repo/references/module-map.md](../doge-shell-repo/references/module-map.md) only when ownership is still unclear.
- If you end up editing, switch to the narrower feature skill before choosing validation.
