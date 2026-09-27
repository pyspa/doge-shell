#!/usr/bin/env python3
"""Small independent review packet from changed paths and the router."""

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
from agent_routing import load_routes, route_context  # noqa: E402


def changed_paths():
    tracked = subprocess.run(["git", "diff", "--name-only", "-z", "HEAD"], cwd=ROOT, capture_output=True, check=True).stdout
    untracked = subprocess.run(["git", "ls-files", "--others", "--exclude-standard", "-z"], cwd=ROOT, capture_output=True, check=True).stdout
    return sorted({p.decode() for p in (tracked + untracked).split(b"\0") if p})


def review_diff():
    tracked = subprocess.run(["git", "diff", "HEAD", "--"], cwd=ROOT, capture_output=True, text=True, check=True).stdout
    untracked = subprocess.run(["git", "ls-files", "--others", "--exclude-standard", "-z"], cwd=ROOT, capture_output=True, check=True).stdout
    additions = []
    for raw in untracked.split(b"\0"):
        if raw:
            path = raw.decode()
            patch = subprocess.run(["git", "diff", "--no-index", "--", "/dev/null", path],
                                   cwd=ROOT, capture_output=True, text=True)
            if patch.returncode not in (0, 1):
                raise RuntimeError(f"cannot diff new file: {path}: {patch.stderr.strip()}")
            additions.append(patch.stdout)
    return tracked + "".join(additions)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--prompt", default="", help="original task prompt included in the packet")
    parser.add_argument("--validation-result", type=Path, help="JSON validation result to include in the review packet")
    args = parser.parse_args()
    paths = changed_paths()
    routed = route_context(load_routes(ROOT / "docs/ai/agent-routing.json"), args.prompt, paths)
    routes = routed["routes"]
    packet = {
        "original_task": args.prompt,
        "risk": routed["risk"],
        "changed_files": paths,
        "diff": review_diff(),
        "router_result": routed,
        "routes": [r["id"] for r in routes],
        "skills": sorted({s["path"] for r in routes for s in r["skills"]} | ({"docs/ai/skills/doge-shell-review/SKILL.md"} if routed["risk"] == "high" else set())),
        "references": sorted({ref for r in routes for ref in r["references"]}),
        "recommended_checks": ["doctor validate", "git diff --check"],
        "validation_result": json.loads(args.validation_result.read_text(encoding="utf-8")) if args.validation_result else None,
    }
    if args.json:
        print(json.dumps(packet, ensure_ascii=False, indent=2))
    else:
        print(f"risk: {packet['risk']}\nroutes: {', '.join(packet['routes'])}\nchanged: {len(paths)} files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
