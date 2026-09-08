---
name: dsh-skill-authoring
description: Use when creating or editing runtime skills, Skill 作成, Skill 更新, or Codex/doge-shell skills from this repository. Helps keep SKILL.md short, put trigger text in frontmatter, and move detail into references or scripts.
---

# DSH Skill Authoring

- Keep `SKILL.md` short and imperative.
- Put all trigger conditions in frontmatter `description`.
- Read [references/checklist.md](references/checklist.md) before finalizing a skill.
- Read [references/layout.md](references/layout.md) when deciding whether content belongs in `SKILL.md`, `references/`, or `scripts/`.
- Use shell or Python for bundled helpers; keep helper scripts short and deterministic.
- A runtime skill the `!` chat reads keeps frontmatter to flat `name:` and `description:`; the reader in `dsh-builtin/src/chatgpt/skills/mod.rs` is not a YAML parser, and only `description` reaches the prompt.
- Repository-specific procedures belong in `<project>/.dsh/skills/`; portable ones in `~/.config/dsh/skills/`. A project skill of the same name shadows a personal one.
- A bundled `scripts/` file always prompts before it runs, at every safety level. That is deliberate; do not design around it.
