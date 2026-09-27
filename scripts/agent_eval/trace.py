"""Runner-neutral process observations. Missing usage remains None."""

from __future__ import annotations

import json
import re
from dataclasses import asdict, dataclass
from pathlib import Path


@dataclass
class NormalizedRunResult:
    runner: str
    runner_version: str
    model: str | None
    agent: str | None
    variant: str | None
    case_id: str
    success: bool
    exit_code: int
    duration_ms: int
    prompt_tokens: int | None
    cached_prompt_tokens: int | None
    completion_tokens: int | None
    cost_usd: float | None
    turn_count: int | None
    tool_calls: int | None
    command_calls: int | None
    files_changed: list[str]
    diff_insertions: int
    diff_deletions: int
    validation_commands: list[str]
    trace_available: bool
    commands: list[str]
    accesses: list[str]

    def to_dict(self) -> dict:
        return asdict(self)


def read_events(path: Path) -> list[dict]:
    events = []
    for line in path.read_text(encoding="utf-8").splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(event, dict):
            events.append(event)
    return events


def diff_size(diff: str) -> tuple[int, int]:
    insertions = deletions = 0
    for line in diff.splitlines():
        if line.startswith("+++") or line.startswith("---"):
            continue
        insertions += line.startswith("+")
        deletions += line.startswith("-")
    return insertions, deletions


def trace_accesses(commands: list[str]) -> list[str]:
    # Store observable path mentions only; do not infer what the model read.
    return sorted(set(re.findall(r"(?:docs/ai/skills|scripts)/[\w./-]+", "\n".join(commands))))
