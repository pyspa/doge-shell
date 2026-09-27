"""Runner contract: native events stop at the normalization boundary."""

from dataclasses import dataclass
from pathlib import Path
from typing import Protocol


@dataclass(frozen=True)
class RunnerCapabilities:
    structured_trace: bool
    usage: bool
    model_override: bool
    session_export: bool
    agent_selection: bool


@dataclass(frozen=True)
class RunRequest:
    worktree: Path
    artifact: Path
    prompt: str
    model: str | None
    agent: str | None
    variant: str | None


@dataclass(frozen=True)
class RawRunResult:
    events_path: Path
    stderr_path: Path
    exit_code: int
    duration_ms: int


class AgentRunner(Protocol):
    def probe(self) -> RunnerCapabilities: ...
    def version(self) -> str: ...
    def run(self, request: RunRequest, timeout: int) -> RawRunResult: ...
    def normalize(self, result: RawRunResult) -> dict: ...
