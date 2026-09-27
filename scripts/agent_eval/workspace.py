"""Temporary detached worktrees; the source checkout is never an eval target."""

from __future__ import annotations

import subprocess
import tempfile
import os
import signal
from contextlib import contextmanager
from pathlib import Path

from .case import ROOT
from .runner import agent_environment


def git(*args: str, cwd: Path = ROOT, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", *args], cwd=cwd, text=True, capture_output=True, check=check)


@contextmanager
def isolated_worktree(base_sha: str):
    with tempfile.TemporaryDirectory(prefix="doge-shell-agent-eval-") as parent:
        path = Path(parent) / "worktree"
        git("worktree", "add", "--detach", str(path), base_sha)
        try:
            yield path
        finally:
            git("worktree", "remove", "--force", str(path))


def apply_mutation(worktree: Path, patch: Path) -> None:
    subprocess.run(["git", "apply", "--check", str(patch)], cwd=worktree, check=True)
    # Stage only the fixture in the disposable worktree. The unstaged diff is
    # then the agent's repair, even when final files equal the original HEAD.
    subprocess.run(["git", "apply", "--index", str(patch)], cwd=worktree, check=True)


def bounded_command(args: list[str], cwd: Path, timeout: int) -> tuple[int | None, str, bool]:
    env = agent_environment("validation")
    proc = subprocess.Popen(args, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True, env=env)
    try:
        output, _ = proc.communicate(timeout=timeout)
        return proc.returncode, output, False
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        output, _ = proc.communicate()
        return None, output, True
    except KeyboardInterrupt:
        os.killpg(proc.pid, signal.SIGKILL)
        proc.communicate()
        raise


def changed_files(worktree: Path) -> list[str]:
    tracked = git("diff", "--name-only", "-z", cwd=worktree).stdout.split("\0")
    untracked = git("ls-files", "--others", "--exclude-standard", "-z", cwd=worktree).stdout.split("\0")
    return sorted({p for p in tracked + untracked if p})


def agent_diff(worktree: Path) -> str:
    diff = git("diff", "--binary", cwd=worktree).stdout
    untracked = git("ls-files", "--others", "--exclude-standard", "-z", cwd=worktree).stdout.split("\0")
    for path in untracked:
        if not path:
            continue
        part = git("diff", "--no-index", "--binary", "--", "/dev/null", str(worktree / path), cwd=worktree, check=False).stdout
        diff += part.replace(str(worktree) + "/", "")
    return diff
