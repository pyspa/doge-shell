"""Claude Code stream-json adapter; unsupported CLI versions are skipped."""

from __future__ import annotations

import subprocess
from pathlib import Path

from .trace import read_events


def capability() -> tuple[bool, str, str]:
    version = subprocess.run(["claude", "--version"], text=True, capture_output=True)
    help_result = subprocess.run(["claude", "--help"], text=True, capture_output=True)
    help_text = help_result.stdout
    required = ("--print", "--output-format", "stream-json", "--max-turns", "--max-budget-usd")
    return all(flag in help_text for flag in required), version.stdout.strip(), help_text


def command(worktree: Path, model: str | None, max_turns: int, budget_usd: float) -> list[str]:
    args = ["claude", "--print", "--output-format", "stream-json", "--verbose", "--max-turns", str(max_turns), "--max-budget-usd", str(budget_usd), "--permission-mode", "acceptEdits"]
    if model:
        args += ["--model", model]
    # With --print and no positional prompt, Claude reads the text from stdin.
    return args


def normalize(events_path: Path) -> dict:
    events = read_events(events_path)
    commands: list[str] = []
    tool_calls = 0
    usage: dict = {}
    final = ""
    model = None
    cost = None
    turns = None
    for event in events:
        kind = event.get("type")
        if kind == "assistant":
            message = event.get("message") or {}
            model = message.get("model") or model
            for block in message.get("content") or []:
                if block.get("type") == "tool_use":
                    tool_calls += 1
                    if block.get("name") == "Bash":
                        commands.append((block.get("input") or {}).get("command", ""))
        if kind == "result":
            final = event.get("result", final)
            usage = event.get("usage") or usage
            cost = event.get("total_cost_usd")
            turns = event.get("num_turns")
    return {
        "commands": commands,
        "tool_calls": tool_calls,
        "prompt_tokens": usage.get("input_tokens"),
        "cached_prompt_tokens": usage.get("cache_read_input_tokens"),
        "completion_tokens": usage.get("output_tokens"),
        "cost_usd": cost,
        "turn_count": turns,
        "final": final,
        "model": model,
        "trace_available": bool(events),
    }
