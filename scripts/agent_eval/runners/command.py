"""Generic argv-only coding-agent runner with no native trace requirement."""

import json
import subprocess
from pathlib import Path

from ..protocol import RawRunResult, RunRequest, RunnerCapabilities
from ..runner import run_process


class CommandRunner:
    def __init__(self, config_path: Path):
        config = json.loads(config_path.read_text(encoding="utf-8"))
        argv = config.get("argv")
        if not isinstance(argv, list) or not argv or not all(isinstance(item, str) and item for item in argv):
            raise ValueError("runner config must contain a non-empty argv string array")
        if not any("{prompt_file}" in item or "{prompt}" in item for item in argv):
            raise ValueError("runner argv must contain {prompt_file} or {prompt}")
        self.argv = argv
        self.env_allowlist = config.get("env_allowlist", [])
        if not isinstance(self.env_allowlist, list) or not all(isinstance(x, str) for x in self.env_allowlist):
            raise ValueError("env_allowlist must be a string array")

    def probe(self) -> RunnerCapabilities:
        return RunnerCapabilities(False, False, any("{model}" in item for item in self.argv), False, False)

    def version(self) -> str:
        try:
            result = subprocess.run([self.argv[0], "--version"], capture_output=True, text=True, timeout=10)
            return (result.stdout or result.stderr).strip() if result.returncode == 0 else "unknown"
        except (OSError, subprocess.TimeoutExpired):
            return "unknown"

    def run(self, request: RunRequest, timeout: int) -> RawRunResult:
        prompt_file = request.artifact / "prompt.txt"
        prompt_file.write_text(request.prompt, encoding="utf-8")
        values = {"prompt_file": str(prompt_file), "prompt": request.prompt, "worktree": str(request.worktree), "model": request.model or ""}
        try:
            argv = [item.format_map(values) for item in self.argv]
        except (KeyError, ValueError) as exc:
            raise ValueError(f"invalid runner placeholder: {exc}") from exc
        events = request.artifact / "raw-events.jsonl"
        stderr = request.artifact / "stderr.log"
        code, duration = run_process(argv, request.prompt, request.worktree, events, stderr, timeout, self.env_allowlist)
        return RawRunResult(events, stderr, code, duration)

    def normalize(self, result: RawRunResult) -> dict:
        return {"commands": [], "tool_calls": None, "prompt_tokens": None,
                "cached_prompt_tokens": None, "completion_tokens": None,
                "cost_usd": None, "turn_count": None, "final": result.events_path.read_text(encoding="utf-8", errors="replace"),
                "model": None, "trace_available": False}
