"""Load a curated mutation case without accepting paths outside the repository."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TASKS = ROOT / "docs/ai/evals/tasks"


@dataclass(frozen=True)
class Case:
    id: str
    category: str
    risk: str
    prompt: str
    mutation: Path
    expected_routes: list[str]
    required_tests: list[str]
    required_checkers: list[str]
    forbidden_commands: list[str]
    allowed_change_prefixes: list[str]
    max_changed_files: int
    deterministic_checks: list[str]


def load_case(case_id: str) -> Case:
    if not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", case_id):
        raise ValueError(f"invalid case id: {case_id}")
    path = TASKS / f"{case_id}.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("version") != 1 or data.get("id") != case_id:
        raise ValueError(f"invalid case version/id: {path}")
    mutation = (ROOT / data["mutation"]).resolve()
    if mutation.parent != (ROOT / "docs/ai/evals/mutations").resolve():
        raise ValueError(f"mutation outside fixture directory: {mutation}")
    return Case(
        id=case_id,
        category=data["category"],
        risk=data["risk"],
        prompt=data["prompt"],
        mutation=mutation,
        expected_routes=data["expected_routes"],
        required_tests=data["required_tests"],
        required_checkers=data.get("required_checkers", []),
        forbidden_commands=data.get("forbidden_commands", []),
        allowed_change_prefixes=data["allowed_change_prefixes"],
        max_changed_files=data["max_changed_files"],
        deterministic_checks=data["deterministic_checks"],
    )
