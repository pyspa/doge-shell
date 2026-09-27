"""OpenCode JSON event adapter; usage is unknown unless events prove it."""

import subprocess

from ..protocol import RawRunResult, RunRequest, RunnerCapabilities
from ..runner import run_process
from ..trace import read_events


class OpenCodeRunner:
    def __init__(self):
        help_result = subprocess.run(["opencode", "run", "--help"], capture_output=True, text=True)
        self.help_text = help_result.stdout if help_result.returncode == 0 else ""

    def probe(self) -> RunnerCapabilities:
        return RunnerCapabilities("--format" in self.help_text, False, "--model" in self.help_text, True, "--agent" in self.help_text)

    def version(self) -> str:
        return subprocess.run(["opencode", "--version"], capture_output=True, text=True, check=True).stdout.strip()

    def run(self, request: RunRequest, timeout: int) -> RawRunResult:
        argv = ["opencode", "run", "--format", "json"]
        if "--standalone" in self.help_text:
            argv.append("--standalone")
        if "--dir" in self.help_text:
            argv += ["--dir", str(request.worktree)]
        if request.model:
            model = request.model
            if request.variant and "--variant" not in self.help_text:
                model += f"#{request.variant}"
            argv += ["--model", model]
        if request.agent:
            argv += ["--agent", request.agent]
        if request.variant and "--variant" in self.help_text:
            argv += ["--variant", request.variant]
        elif request.variant and not request.model:
            raise ValueError("this OpenCode version needs --model with --variant")
        argv.append(request.prompt)
        events = request.artifact / "raw-events.jsonl"
        stderr = request.artifact / "stderr.log"
        code, duration = run_process(argv, "", request.worktree, events, stderr, timeout)
        return RawRunResult(events, stderr, code, duration)

    def normalize(self, result: RawRunResult) -> dict:
        events = read_events(result.events_path)
        commands = []
        tool_calls = 0
        final = ""
        session_id = None
        model = None
        for event in events:
            current_session = event.get("sessionID") or event.get("sessionId")
            if isinstance(current_session, str):
                session_id = current_session
            part = event.get("part") or {}
            if not isinstance(part, dict):
                continue
            if part.get("type") == "tool":
                tool_calls += 1
                if part.get("tool") == "bash":
                    state = part.get("state") or {}
                    tool_input = state.get("input") if isinstance(state, dict) else None
                    if isinstance(tool_input, dict) and isinstance(tool_input.get("command"), str):
                        commands.append(tool_input["command"])
            if part.get("type") == "text":
                final = part.get("text", final)
            if isinstance(event.get("model"), str):
                model = event["model"]
        return {"commands": commands, "tool_calls": tool_calls if events else None,
                "prompt_tokens": None, "cached_prompt_tokens": None, "completion_tokens": None,
                "cost_usd": None, "turn_count": None, "final": final, "model": model,
                "trace_available": bool(events), "session_id": session_id}

    def export_session(self, session_id: str, artifact) -> None:
        result = subprocess.run(["opencode", "session", "export", session_id, "--sanitize"],
                                capture_output=True, text=True, timeout=30)
        if result.returncode == 0:
            (artifact / "session.json").write_text(result.stdout, encoding="utf-8")
        else:
            (artifact / "session-export-error.log").write_text(result.stderr, encoding="utf-8")
