#!/usr/bin/env python3
"""Portable, short-lived task contract in ignored .agent/TASK.md."""

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TASK = ROOT / ".agent/TASK.md"
TEMPLATE = ROOT / "docs/ai/templates/TASK.md"
SECTIONS = ["Goal", "Done when", "Non-goals", "Hard constraints", "Relevant routes / skills", "Expected scope", "Milestones", "Validation", "Decisions", "Current state", "Blockers"]


def sections(text):
    chunks = re.split(r"(?m)^# (.+)\n", text)
    return {chunks[i]: chunks[i + 1].strip() for i in range(1, len(chunks) - 1, 2)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    init = sub.add_parser("init")
    init.add_argument("--goal", required=True)
    init.add_argument("--risk", choices=("high", "normal"), default="normal")
    sub.add_parser("validate")
    sub.add_parser("status")
    sub.add_parser("clear")
    args = parser.parse_args()
    if args.command == "init":
        if TASK.exists():
            print(f"already exists: {TASK}", file=sys.stderr)
            return 1
        TASK.parent.mkdir(exist_ok=True)
        text = TEMPLATE.read_text(encoding="utf-8")
        text = text.replace("# Goal\n\n", f"# Goal\n\n{args.goal}\n\n", 1)
        text = text.replace("# Hard constraints\n\n", f"# Hard constraints\n\n- Risk: {args.risk}\n\n", 1)
        TASK.write_text(text, encoding="utf-8")
        print(TASK)
        return 0
    if not TASK.exists():
        print("no .agent/TASK.md", file=sys.stderr)
        return 1
    if args.command == "clear":
        TASK.unlink()
        print("cleared .agent/TASK.md")
        return 0
    data = sections(TASK.read_text(encoding="utf-8"))
    if args.command == "validate":
        missing = [name for name in SECTIONS if name not in data]
        empty = [name for name in ("Goal", "Done when", "Hard constraints") if not data.get(name)]
        current_bullets = sum(line.lstrip().startswith(("- ", "* ")) for line in data.get("Current state", "").splitlines())
        if missing or empty or current_bullets > 10:
            print(f"missing sections: {missing}; empty required sections: {empty}; current state bullets: {current_bullets}/10", file=sys.stderr)
            return 1
        print("task contract valid")
    else:
        print(f"Goal: {data.get('Goal', '').splitlines()[0] if data.get('Goal') else '?'}")
        print(f"Current state: {len(data.get('Current state', '').splitlines())} lines")
        print(f"Blockers: {len(data.get('Blockers', '').splitlines())} lines")
    return 0


if __name__ == "__main__":
    sys.exit(main())
