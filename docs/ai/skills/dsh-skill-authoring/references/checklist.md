# Checklist

- `name` uses lowercase letters, digits, and hyphens only.
- `description` explains both capability and trigger context.
- `description` stays under 240 characters - the dsh `!` chat runtime truncates the
  prompt line there (`MAX_SKILL_SUMMARY_CHARS`); `skill_manage` refuses a write past
  300.
- `SKILL.md` body contains procedure, not long background.
- Detailed examples, module maps, or schemas live in `references/`.
- Repeated deterministic steps live in `scripts/`.
- Paths mentioned from `SKILL.md` point directly to needed files.
- Run `scripts/check-ai-guidance.sh` after editing AGENTS, docs/ai, or Skill files.
