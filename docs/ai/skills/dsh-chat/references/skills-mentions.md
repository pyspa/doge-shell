# Skills and mentions

- The runtime reads at most three roots, most authoritative first: `<project>/.dogesh/skills`, `<project>/.agents/skills` (read-only), then the user directory (`~/.config/dogesh/skills`). A project skill of the same name shadows a personal one.
- Only `name` and `description` reach the prompt - one line per skill. The body is read on demand.
- Mention skills explicitly with leading `@name` tokens, up to 5. Parsing stops at the first non-skill token, so put mentions first.
- A project root is untrusted data: the shell asks once per directory before its descriptions enter the prompt (`y` for the session, `a` to remember). An untrusted root is skipped rather than prompted inside an unattended task.
- Manage personal skills with `skill list`, `skill show <name>` and `skill remove <name>`. A skill the chat proposes itself may wait for review depending on staging - `skill pending` / `skill diff` / `skill approve` / `skill reject` land it.
- Writing a new skill is $dsh-skill-authoring territory, not this skill's.
