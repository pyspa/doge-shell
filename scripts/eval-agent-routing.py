#!/usr/bin/env python3
"""Run deterministic routing fixtures without invoking the CLI."""

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))

from agent_routing import load_routes, route_context, validate_manifest


def main():
    manifest = load_routes(ROOT / "docs/ai/agent-routing.json")
    errors = validate_manifest(manifest, ROOT)
    cases = json.loads((ROOT / "docs/ai/evals/routing-cases.json").read_text(encoding="utf-8"))
    passed = 0
    for case in cases:
        result = route_context(manifest, case.get("topic", ""), case.get("paths", []))
        routes = result["routes"]
        skills = {skill["name"] for route in routes for skill in route["skills"]}
        skill_paths_valid = all(skill["path"] == f"docs/ai/skills/{skill['name']}/SKILL.md" for route in routes for skill in route["skills"])
        refs = {ref for route in routes for ref in route["references"]}
        valid = (routes[0]["id"] == case["expected_primary_route"]
                 and set(case.get("expected_skills", [])) <= skills
                 and skill_paths_valid
                 and set(case.get("required_references", [])) <= refs
                 and len(routes) <= case.get("max_routes", 2)
                 and result == route_context(manifest, case.get("topic", ""), case.get("paths", [])))
        if valid:
            passed += 1
        else:
            errors.append(f"{case['id']}: expected {case['expected_primary_route']}, got {[r['id'] for r in routes]}")
    if errors:
        print("\n".join(errors), file=sys.stderr)
    print(f"routing eval: {passed}/{len(cases)} passed")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
