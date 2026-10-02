---
name: doge-shell-investigation
description: Use for investigation, audit, root-cause analysis, performance inspection, 調査, 監査, 原因調査, or 性能確認 in doge-shell before editing. Keeps work read-only first, avoids broad tests early, and narrows likely files.
---

# Doge Shell Investigation

- Start with `rg --files` or `rg -n`; do not edit or run broad tests first.
- Read [../doge-shell-repo/references/read-boundaries.md](../doge-shell-repo/references/read-boundaries.md) before opening `README.md` or running workspace-wide commands.
- Reuse the router result and read its domain skill. Use [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) only for fallback; search the relevant entry.
- State the observed symptom, a minimal reproduction, and the next hypothesis to distinguish. Trace the entry point, owning state, and existing regression test before expanding the search.
- Keep logs in a temporary file when large; inspect the exit status and first relevant failure. Do not rerun an unchanged failing command without new evidence or changed conditions.
- Use [../doge-shell-repo/references/module-map.md](../doge-shell-repo/references/module-map.md) only when ownership is still unclear.
- If you end up editing, switch to the narrower feature skill before choosing validation.
- Independent review of an existing diff belongs to [doge-shell-review](../doge-shell-review/SKILL.md), not this skill.
