"""Codex JSONL adapter."""

from __future__ import annotations

import subprocess
from pathlib import Path

from .trace import read_events


def version() -> str:
    return subprocess.run(["codex", "--version"], text=True, capture_output=True, check=True).stdout.strip()


def command(worktree: Path, model: str | None) -> list[str]:
    args = ["codex", "exec", "--json", "--sandbox", "workspace-write", "--cd", str(worktree), "--ephemeral"]
    if model:
        args += ["--model", model]
    return args + ["-"]


def normalize(events_path: Path) -> dict:
    events = read_events(events_path)
    commands: list[str] = []
    tool_calls = 0
    usage: dict = {}
    final = ""
    model = None
    for event in events:
        kind = event.get("type")
        item = event.get("item") or {}
        if kind == "item.started":
            tool_calls += item.get("type") not in ("agent_message", "reasoning")
            if item.get("type") == "command_execution":
                commands.append(item.get("command", ""))
        if kind == "item.completed" and item.get("type") == "agent_message":
            final = item.get("text", final)
        if kind == "turn.completed":
            usage = event.get("usage") or usage
        if kind == "thread.started":
            model = event.get("model") or model
    return {
        "commands": commands,
        "tool_calls": tool_calls,
        "prompt_tokens": usage.get("input_tokens"),
        "cached_prompt_tokens": usage.get("cached_input_tokens"),
        "completion_tokens": usage.get("output_tokens"),
        "cost_usd": None,
        "turn_count": None,
        "final": final,
        "model": model,
        "trace_available": bool(events),
    }
