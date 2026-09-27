"""Claude runner; plain print is a visible fallback without trace or usage."""

from .. import claude
from ..protocol import RawRunResult, RunRequest, RunnerCapabilities
from ..runner import run_process


class ClaudeRunner:
    def __init__(self, max_turns: int, budget_usd: float):
        self.max_turns = max_turns
        self.budget_usd = budget_usd
        self.structured, self.cli_version, _ = claude.capability()

    def probe(self) -> RunnerCapabilities:
        return RunnerCapabilities(self.structured, self.structured, True, False, False)

    def version(self) -> str:
        return self.cli_version

    def run(self, request: RunRequest, timeout: int) -> RawRunResult:
        argv = claude.command(request.worktree, request.model, self.max_turns, self.budget_usd) if self.structured else ["claude", "--print"]
        events = request.artifact / "raw-events.jsonl"
        stderr = request.artifact / "stderr.log"
        code, duration = run_process(argv, request.prompt, request.worktree, events, stderr, timeout)
        return RawRunResult(events, stderr, code, duration)

    def normalize(self, result: RawRunResult) -> dict:
        if self.structured:
            return claude.normalize(result.events_path)
        return {"commands": [], "tool_calls": None, "prompt_tokens": None,
                "cached_prompt_tokens": None, "completion_tokens": None,
                "cost_usd": None, "turn_count": None,
                "final": result.events_path.read_text(encoding="utf-8", errors="replace"),
                "model": None, "trace_available": False}
