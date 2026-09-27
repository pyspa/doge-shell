"""Codex runner; native event parsing stays inside this adapter."""

from .. import codex
from ..protocol import RawRunResult, RunRequest, RunnerCapabilities
from ..runner import run_process


class CodexRunner:
    def probe(self) -> RunnerCapabilities:
        return RunnerCapabilities(True, True, True, False, False)

    def version(self) -> str:
        return codex.version()

    def run(self, request: RunRequest, timeout: int) -> RawRunResult:
        events = request.artifact / "raw-events.jsonl"
        stderr = request.artifact / "stderr.log"
        code, duration = run_process(codex.command(request.worktree, request.model), request.prompt,
                                     request.worktree, events, stderr, timeout)
        return RawRunResult(events, stderr, code, duration)

    def normalize(self, result: RawRunResult) -> dict:
        return codex.normalize(result.events_path)
