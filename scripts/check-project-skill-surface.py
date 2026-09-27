#!/usr/bin/env python3
"""Keep project skill discovery small and identical across agents."""

from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORE = {"doge-shell-repo", "doge-shell-validation", "doge-shell-investigation"}


def check() -> list[str]:
    errors = []
    for parent in (ROOT / ".agents/skills", ROOT / ".claude/skills"):
        if parent.is_symlink() or not parent.is_dir():
            errors.append(f"{parent.relative_to(ROOT)} must be a directory")
            continue
        entries = {entry.name: entry for entry in parent.iterdir()}
        if set(entries) != CORE:
            errors.append(f"{parent.relative_to(ROOT)}: expected {sorted(CORE)}, found {sorted(entries)}")
        for name, entry in entries.items():
            target = ROOT / "docs/ai/skills" / name
            if not entry.is_symlink() or not entry.exists() or entry.resolve() != target.resolve():
                errors.append(f"{entry.relative_to(ROOT)} must link to {target.relative_to(ROOT)}")
    return errors


if __name__ == "__main__":
    issues = check()
    if issues:
        raise SystemExit("\n".join(issues))
    print("ok project skill surface")
