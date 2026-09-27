#!/usr/bin/env python3
"""Fail when always-on repository guidance exceeds its small static budget."""

import re
import importlib.util
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("skill_surface", ROOT / "scripts/check-project-skill-surface.py")
surface = importlib.util.module_from_spec(spec)
spec.loader.exec_module(surface)
LIMITS = {"AGENTS.md bytes": 6 * 1024, "AGENTS.md lines": 140,
          "portable exposed skills": 4, "total description chars": 1500,
          "individual description chars": 300, "CLAUDE.md bytes": 3 * 1024}


def description(path: Path) -> str:
    content = path.read_text(encoding="utf-8")
    match = re.search(r"(?m)^description: (.+)$", content.split("---", 2)[1])
    if not match:
        raise ValueError(f"missing description: {path}")
    return match.group(1)


def main() -> int:
    issues = surface.check()
    if issues:
        print("\n".join(issues))
        return 1
    agents = (ROOT / "AGENTS.md").read_bytes()
    claude = (ROOT / "CLAUDE.md").read_bytes()
    skills = list((ROOT / ".agents/skills").iterdir())
    descriptions = [description(skill / "SKILL.md") for skill in skills]
    actual = {"AGENTS.md bytes": len(agents), "AGENTS.md lines": len(agents.splitlines()),
              "portable exposed skills": len(skills), "total description chars": sum(map(len, descriptions)),
              "individual description chars": max(map(len, descriptions), default=0),
              "CLAUDE.md bytes": len(claude)}
    errors = 0
    for name, value in actual.items():
        print(f"{name}: {value}/{LIMITS[name]}")
        errors += value > LIMITS[name]
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
