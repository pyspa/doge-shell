#!/usr/bin/env python3
"""Pre-edit router CLI: one command from a task to narrow skills.

Usage:

    python3 scripts/agent-context.py --topic "wait -n ..." --json
    python3 scripts/agent-context.py --path dsh/src/proxy/builtin/jobs/wait/mod.rs --json
    python3 scripts/agent-context.py --changed --json

This is the editing-before half of the workflow. It never suggests
validation commands; after editing, use the canonical test-scope.md
(or doctor validate from a fresh release binary). No validation mappings
are duplicated here.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "scripts"))

import agent_routing  # noqa: E402

MANIFEST_PATH = REPO_ROOT / "docs/ai/agent-routing.json"


def changed_paths() -> list[str]:
    """Repo-relative paths from `git status --short` (tracked + untracked)."""
    proc = subprocess.run(
        ["git", "status", "--porcelain", "--", "."],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        print(f"error: git status failed: {proc.stderr.strip()}", file=sys.stderr)
        sys.exit(1)
    paths: list[str] = []
    for line in proc.stdout.splitlines():
        if len(line) < 4:
            continue
        entry = line[3:].strip().strip('"')
        # Renames report "old -> new"; the new path is what matters.
        if " -> " in entry:
            entry = entry.rsplit(" -> ", 1)[1].strip().strip('"')
        if entry:
            paths.append(entry)
    return paths


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--topic", default="", help="free-text task description")
    parser.add_argument(
        "--path",
        action="append",
        default=[],
        dest="paths",
        help="repo-relative path hint (repeatable)",
    )
    parser.add_argument(
        "--changed",
        action="store_true",
        help="use git status paths as path hints",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="emit machine-readable JSON (default is human-readable)",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)

    try:
        manifest = agent_routing.load_routes(MANIFEST_PATH)
    except (OSError, ValueError) as exc:
        print(f"error: cannot read routing manifest: {exc}", file=sys.stderr)
        return 1

    errors = agent_routing.validate_manifest(manifest, REPO_ROOT)
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    paths = list(args.paths)
    if args.changed:
        paths.extend(changed_paths())

    normalized_paths = []
    for raw in paths:
        path = Path(raw)
        if path.is_absolute():
            try:
                raw = str(path.resolve().relative_to(REPO_ROOT))
            except ValueError:
                print(f"error: path is outside repository: {raw}", file=sys.stderr)
                return 1
        normalized_paths.append(raw)

    result = agent_routing.route_context(manifest, topic=args.topic, paths=normalized_paths)

    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0

    print(f"risk: {result['risk']}")
    for route in result["routes"]:
        print(f"- {route['id']} (score {route['score']})")
        print(f"  skills: {', '.join(skill['path'] for skill in route['skills'])}")
        print(f"  references: {', '.join(route['references'])}")
        if route["matched_terms"]:
            print(f"  matched terms: {', '.join(route['matched_terms'])}")
        if route["matched_paths"]:
            print(f"  matched paths: {', '.join(route['matched_paths'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
